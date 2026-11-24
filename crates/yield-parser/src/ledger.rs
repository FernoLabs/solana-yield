//! Extract measurements from an admitted, resolved Solana transaction.
//! The caller verifies the block source, transaction position and finality.

mod logs;

use std::fmt;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::{jupiter_ledger, kamino_ledger, LedgerMeasurement};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStatus {
    Complete,
    Unavailable,
    Truncated,
    Invalid,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnStatus {
    Decoded,
    Unavailable,
    Invalid,
    NotApplicable,
}

#[derive(Debug, Serialize)]
pub struct TransactionFacts {
    pub execution_status: ExecutionStatus,
    pub log_status: LogStatus,
    pub kamino_return_status: ReturnStatus,
    pub measurements: Vec<LocatedMeasurement>,
}

#[derive(Debug, Serialize)]
pub struct LocatedMeasurement {
    pub log_index: Option<usize>,
    pub outer_instruction_index: Option<usize>,
    pub earn_market: Option<jupiter_ledger::EarnMarket>,
    pub measurement: LedgerMeasurement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedgerError;

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid transaction envelope")
    }
}

impl std::error::Error for LedgerError {}

pub(crate) fn serialize_key<S: Serializer>(key: &[u8; 32], out: S) -> Result<S::Ok, S::Error> {
    out.serialize_str(&bs58::encode(key).into_string())
}

pub(crate) fn serialize_u64<S: Serializer>(value: &u64, out: S) -> Result<S::Ok, S::Error> {
    out.serialize_str(&value.to_string())
}

pub(crate) fn serialize_u128<S: Serializer>(value: &u128, out: S) -> Result<S::Ok, S::Error> {
    out.serialize_str(&value.to_string())
}

/// Accept the resolved JSON transaction form with message instructions and
/// metadata. A missing or invalid log trace adds no log measurements.
pub fn parse_transaction(tx: &Value) -> Result<TransactionFacts, LedgerError> {
    if !tx
        .pointer("/transaction/message")
        .is_some_and(Value::is_object)
    {
        return Err(LedgerError);
    }
    let meta = tx.get("meta");
    let execution_status = match meta.and_then(|meta| meta.get("err")) {
        Some(Value::Null) => ExecutionStatus::Succeeded,
        Some(_) => ExecutionStatus::Failed,
        None => ExecutionStatus::Unknown,
    };
    let mut result = TransactionFacts {
        execution_status,
        log_status: LogStatus::NotApplicable,
        kamino_return_status: ReturnStatus::NotApplicable,
        measurements: Vec::new(),
    };
    if execution_status != ExecutionStatus::Succeeded {
        return Ok(result);
    }

    match logs::parse(meta.and_then(|meta| meta.get("logMessages"))) {
        Ok(trace) => {
            result.log_status = LogStatus::Complete;
            let market = (trace.earn_invocations == 1 && trace.earn_top_level)
                .then(|| bound_earn_market(tx))
                .flatten();
            // The same committed Earn call can emit more than one rate update.
            result
                .measurements
                .extend(trace.events.into_iter().map(|event| {
                    let (index, binding) = match &event.measurement {
                        LedgerMeasurement::JupiterEarnExchangePrices { .. } => market
                            .as_ref()
                            .map(|(index, market)| (Some(*index), Some(*market)))
                            .unwrap_or((None, None)),
                        _ => (None, None),
                    };
                    LocatedMeasurement {
                        log_index: Some(event.log_index),
                        outer_instruction_index: index,
                        earn_market: binding,
                        measurement: event.measurement,
                    }
                }));
        }
        Err(status) => result.log_status = status,
    }
    let (status, measurement) = kamino_return(tx);
    result.kamino_return_status = status;
    if let Some((index, measurement)) = measurement {
        result.measurements.push(LocatedMeasurement {
            log_index: None,
            outer_instruction_index: Some(index),
            earn_market: None,
            measurement,
        });
    }
    Ok(result)
}

struct Message<'a> {
    keys: Vec<&'a str>,
    instructions: &'a [Value],
}

impl<'a> Message<'a> {
    fn read(tx: &'a Value) -> Option<Self> {
        let message = tx.pointer("/transaction/message")?;
        let mut keys = Vec::new();
        for list in [
            Some(message.get("accountKeys")?),
            tx.pointer("/meta/loadedAddresses/writable"),
            tx.pointer("/meta/loadedAddresses/readonly"),
        ]
        .into_iter()
        .flatten()
        {
            for key in list.as_array()? {
                keys.push(key.as_str()?);
                if keys.len() > 256 {
                    return None;
                }
            }
        }
        let instructions = message.get("instructions")?.as_array()?;
        if instructions.len() > 256 {
            return None;
        }
        Some(Self { keys, instructions })
    }

    fn address(&self, index: &Value) -> Option<&'a str> {
        let index = usize::try_from(index.as_u64()?).ok()?;
        self.keys.get(index).copied()
    }

    fn program(&self, instruction: &Value) -> Option<&'a str> {
        self.address(instruction.get("programIdIndex")?)
    }

    fn instruction(&self, instruction: &Value) -> Option<(Vec<u8>, Vec<&'a str>)> {
        let data = instruction.get("data")?.as_str()?;
        let accounts = instruction.get("accounts")?.as_array()?;
        if data.len() > 128 || accounts.len() > 64 {
            return None;
        }
        Some((
            bs58::decode(data).into_vec().ok()?,
            accounts
                .iter()
                .map(|account| self.address(account))
                .collect::<Option<_>>()?,
        ))
    }
}

fn bound_earn_market(tx: &Value) -> Option<(usize, jupiter_ledger::EarnMarket)> {
    let message = Message::read(tx)?;
    let mut binding = None;
    for (index, instruction) in message.instructions.iter().enumerate() {
        if message.program(instruction)? != jupiter_ledger::EARN_PROGRAM {
            continue;
        }
        if binding.is_some() {
            return None;
        }
        let (data, accounts) = message.instruction(instruction)?;
        binding = Some((index, jupiter_ledger::earn_market(&data, &accounts).ok()??));
    }
    binding
}

fn kamino_return(tx: &Value) -> (ReturnStatus, Option<(usize, LedgerMeasurement)>) {
    use ReturnStatus::{Decoded, Invalid, NotApplicable, Unavailable};
    let Some(message) = Message::read(tx) else {
        return (NotApplicable, None);
    };
    let Some((index, instruction)) = message.instructions.iter().enumerate().next_back() else {
        return (NotApplicable, None);
    };
    if message.program(instruction) != Some(kamino_ledger::PROGRAM) {
        return (NotApplicable, None);
    }
    let Some((instruction, accounts)) = message.instruction(instruction) else {
        return (NotApplicable, None);
    };
    if !kamino_ledger::matches_getter(&instruction) {
        return (NotApplicable, None);
    }
    if !tx
        .pointer("/meta/innerInstructions")
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().all(|group| {
                let Some(parent) = group.get("index").and_then(Value::as_u64) else {
                    return false;
                };
                let Some(instructions) = group.get("instructions").and_then(Value::as_array) else {
                    return false;
                };
                parent < index as u64 || (parent == index as u64 && instructions.is_empty())
            })
        })
    {
        return (Unavailable, None);
    }
    let Some(return_data) = tx
        .pointer("/meta/returnData")
        .filter(|value| !value.is_null())
    else {
        return (Unavailable, None);
    };
    let (Some(program), Some(encoded), Some("base64")) = (
        return_data.get("programId").and_then(Value::as_str),
        return_data.pointer("/data/0").and_then(Value::as_str),
        return_data.pointer("/data/1").and_then(Value::as_str),
    ) else {
        return (Invalid, None);
    };
    if encoded.len() > 24 {
        return (Invalid, None);
    }
    let Ok(bytes) = STANDARD.decode(encoded) else {
        return (Invalid, None);
    };
    match kamino_ledger::decode(&instruction, &accounts, program, &bytes) {
        Ok(Some(measurement)) => (Decoded, Some((index, measurement))),
        _ => (Invalid, None),
    }
}

#[cfg(test)]
mod tests;
