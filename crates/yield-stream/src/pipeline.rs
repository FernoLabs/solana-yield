//! Instruction-first parsing. Log coverage never decides candidate admission.
use crate::store::{ParsedBlock, ParsedCandidate, ParsedEvent, ParsedInstruction, ParserRejection};
use anyhow::{ensure, Result};
use blockzilla_bank_client::Frame;
use blockzilla_decoder::{decode_bank_financial_block, DecodeOptions, FinancialInterest};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use yield_indexer::{
    instruction_parser::parse_jupiter_instruction,
    stream_parser::{parse_jupiter_logs, JUPITER_LIQUIDITY_PROGRAM},
};

pub const PARSER_VERSION: &str = "jupiter-liquidity-instructions-v1";
fn bounded_reason(value: impl ToString) -> String {
    let mut reason = value.to_string();
    let mut boundary = reason.len().min(512);
    while !reason.is_char_boundary(boundary) {
        boundary -= 1;
    }
    reason.truncate(boundary);
    reason
}
pub fn parse_block(frame: &Frame) -> Result<ParsedBlock> {
    let key: [u8; 32] = bs58::decode(JUPITER_LIQUIDITY_PROGRAM)
        .into_vec()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid parser program key"))?;
    let mut interest = FinancialInterest::from_raw(BTreeSet::from([key]), BTreeSet::new());
    parse_projected_block(decode_bank_financial_block(
        frame,
        &DecodeOptions::default(),
        &mut interest,
    )?)
}
/// Shared entry for admitted live/archive block projections. Source adapters own
/// block admission, coverage and finality. A program key alone is not a call.
pub fn parse_projected_block(decoded: blockzilla_decoder::DecodedBlock) -> Result<ParsedBlock> {
    let mut parsed = ParsedBlock {
        block_time: decoded.block.block_time,
        transactions_scanned: decoded.total_transactions,
        transactions_selected: 0,
        transactions_succeeded: 0,
        transactions_failed: 0,
        transactions_without_metadata: 0,
        parser_rejections: 0,
        parser_rejection_details: Vec::new(),
        candidates: Vec::new(),
        instructions: Vec::new(),
        events: Vec::new(),
    };
    let transactions = decoded.block.transactions.unwrap_or_default();
    ensure!(
        transactions.len() == decoded.transaction_indices.len(),
        "transaction index mismatch"
    );
    for (transaction, index) in transactions.into_iter().zip(decoded.transaction_indices) {
        let (candidate, instructions, events, log_failure) =
            parse_candidate(serde_json::to_value(transaction)?, index as usize)?;
        parsed.transactions_selected += 1;
        match candidate.execution_status.as_str() {
            "succeeded" => parsed.transactions_succeeded += 1,
            "failed" => parsed.transactions_failed += 1,
            _ => parsed.transactions_without_metadata += 1,
        }
        parsed.candidates.push(candidate);
        parsed.instructions.extend(instructions);
        parsed.events.extend(events);
        if let Some(failure) = log_failure {
            // Legacy counter: optional log enrichment failure, not candidate loss.
            parsed.parser_rejections += 1;
            parsed.parser_rejection_details.push(failure);
        }
    }
    Ok(parsed)
}
pub type CandidateResult = (
    ParsedCandidate,
    Vec<ParsedInstruction>,
    Vec<ParsedEvent>,
    Option<ParserRejection>,
);
/// Outer arguments are requests. CPI metadata records invocation attempts;
/// transaction success alone does not prove each CPI's committed state effects.
pub fn parse_candidate(mut value: Value, transaction_index: usize) -> Result<CandidateResult> {
    let signature = value
        .pointer("/transaction/signatures/0")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing transaction signature"))?
        .to_owned();
    let execution = match value
        .get("meta")
        .and_then(Value::as_object)
        .and_then(|m| m.get("err"))
    {
        Some(Value::Null) => "succeeded",
        Some(_) => "failed",
        None => "unknown",
    };
    let instructions = parse_instructions(&value, &signature, transaction_index, execution)?;
    let mut events = Vec::new();
    let mut failure = None;
    let (log_status, log_reason) =
        if let Some(logs) = value.pointer("/meta/logMessages").and_then(Value::as_array) {
            let lines = logs
                .iter()
                .map(|line| line.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>();
            if let Some(lines) = lines {
                if lines
                    .iter()
                    .any(|line| line == "Log truncated" || line == "Program log: Log truncated")
                {
                    (
                        "truncated",
                        Some("transaction logs were truncated".to_owned()),
                    )
                } else if execution == "succeeded" {
                    match parse_jupiter_logs(&lines) {
                        Ok(found) => {
                            events.extend(found.into_iter().map(|event| ParsedEvent {
                                signature: signature.clone(),
                                transaction_index,
                                event,
                            }));
                            ("complete", None)
                        }
                        Err(error) => ("invalid", Some(bounded_reason(error))),
                    }
                } else {
                    ("complete", None)
                }
            } else {
                ("invalid", Some("invalid_execution_logs".into()))
            }
        } else {
            ("unavailable", Some("missing_execution_logs".into()))
        };
    if execution == "succeeded" {
        if let Some(reason) = &log_reason {
            failure = Some(ParserRejection {
                signature: signature.clone(),
                transaction_index,
                reason: reason.clone(),
            });
        }
    }
    // Keep instructions, balances, runtime addresses, errors and CPI metadata.
    // Optional log text is not required for durable candidate storage.
    if let Some(meta) = value.get_mut("meta").and_then(Value::as_object_mut) {
        meta.remove("logMessages");
    }
    Ok((
        ParsedCandidate {
            signature,
            transaction_index,
            execution_status: execution.into(),
            log_status: log_status.into(),
            log_reason,
            parser_version: PARSER_VERSION.into(),
            transaction: value,
        },
        instructions,
        events,
        failure,
    ))
}
fn parse_instructions(
    value: &Value,
    signature: &str,
    transaction_index: usize,
    execution: &str,
) -> Result<Vec<ParsedInstruction>> {
    let message = value
        .pointer("/transaction/message")
        .ok_or_else(|| anyhow::anyhow!("missing transaction message"))?;
    let key_values = message["accountKeys"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing runtime account keys"))?;
    let parsed_keys = key_values.first().is_some_and(Value::is_object);
    let mut keys = key_values
        .iter()
        .map(|key| {
            key.as_str()
                .or_else(|| key.get("pubkey").and_then(Value::as_str))
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid runtime key"))
        })
        .collect::<Result<Vec<_>>>()?;
    if !parsed_keys {
        for kind in ["writable", "readonly"] {
            if let Some(loaded) = value
                .pointer(&format!("/meta/loadedAddresses/{kind}"))
                .and_then(Value::as_array)
            {
                for key in loaded {
                    keys.push(
                        key.as_str()
                            .ok_or_else(|| anyhow::anyhow!("invalid loaded key"))?
                            .into(),
                    );
                }
            }
        }
    }
    ensure!(keys.len() <= 256, "too many runtime keys");
    let outer = message["instructions"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing instruction list"))?;
    let mut result = Vec::new();
    for (index, instruction) in outer.iter().enumerate() {
        if let Some(instruction) = decode_instruction(
            instruction,
            &keys,
            signature,
            transaction_index,
            index,
            None,
            execution,
        )? {
            result.push(instruction);
        }
    }
    if let Some(groups) = value
        .pointer("/meta/innerInstructions")
        .and_then(Value::as_array)
    {
        let mut seen = BTreeSet::new();
        for group in groups {
            let index = group["index"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("invalid CPI outer index"))?
                as usize;
            ensure!(
                index < outer.len() && seen.insert(index),
                "invalid or duplicate CPI group"
            );
            let inner = group["instructions"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("missing CPI instructions"))?;
            for (inner_index, instruction) in inner.iter().enumerate() {
                if let Some(instruction) = decode_instruction(
                    instruction,
                    &keys,
                    signature,
                    transaction_index,
                    index,
                    Some(inner_index),
                    execution,
                )? {
                    result.push(instruction);
                }
            }
        }
    }
    result.sort_by_key(|instruction| {
        (
            instruction.outer_instruction_index,
            instruction
                .inner_instruction_index
                .map(|i| i + 1)
                .unwrap_or(0),
        )
    });
    Ok(result)
}
fn decode_instruction(
    value: &Value,
    keys: &[String],
    signature: &str,
    transaction_index: usize,
    outer_index: usize,
    inner_index: Option<usize>,
    execution: &str,
) -> Result<Option<ParsedInstruction>> {
    let program = value.get("programId").and_then(Value::as_str).or_else(|| {
        value
            .get("programIdIndex")
            .and_then(Value::as_u64)
            .and_then(|i| keys.get(i as usize).map(String::as_str))
    });
    if program != Some(JUPITER_LIQUIDITY_PROGRAM) {
        return Ok(None);
    }
    let data = value
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let accounts = value
        .get("accounts")
        .and_then(Value::as_array)
        .map(|accounts| {
            accounts
                .iter()
                .map(|account| {
                    account
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| account.as_u64().and_then(|i| keys.get(i as usize).cloned()))
                        .ok_or_else(|| anyhow::anyhow!("instruction account address unavailable"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .unwrap_or_else(|| Err(anyhow::anyhow!("instruction account list unavailable")));
    let mut fact = ParsedInstruction {
        signature: signature.into(),
        transaction_index,
        outer_instruction_index: outer_index,
        inner_instruction_index: inner_index,
        stack_height: value
            .get("stackHeight")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        program: JUPITER_LIQUIDITY_PROGRAM.into(),
        name: "unknown".into(),
        execution_status: match (execution, inner_index) {
            ("failed", _) => "failed_transaction",
            ("unknown", _) => "unknown_transaction",
            ("succeeded", None) => "committed_top_level",
            _ => "attempted_cpi_unproven",
        }
        .into(),
        data_base58: data.clone(),
        accounts: Vec::new(),
        arguments: json!({"requested_only":true}),
        decode_status: "unsupported".into(),
        decode_reason: None,
    };
    let parsed = (|| -> Result<_> {
        fact.accounts = accounts?;
        ensure!(
            data.len() <= 4096,
            "instruction data exceeds protocol limit"
        );
        let bytes = bs58::decode(&data).into_vec()?;
        parse_jupiter_instruction(&bytes, &fact.accounts)
    })();
    match parsed {
        Ok(Some(instruction)) => {
            fact.name = instruction.name.clone();
            fact.arguments = serde_json::to_value(instruction)?;
            fact.decode_status = "decoded".into();
        }
        Ok(None) => fact.decode_reason = Some("unsupported_instruction_discriminator".into()),
        Err(error) => {
            fact.decode_status = "invalid".into();
            fact.decode_reason = Some(bounded_reason(error));
        }
    }
    Ok(Some(fact))
}
