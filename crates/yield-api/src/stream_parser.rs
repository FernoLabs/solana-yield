//! Optional Jupiter Lend event enrichment from execution logs.
//! The primary transaction parser is `instruction_parser`; incomplete logs
//! must not remove its candidates or instruction records.
//!
//! Layout source: the official Jupiter IDL at commit
//! `33a22cf7a5bfdd32ab1712dda4adfbeb9b348ad9`:
//! <https://github.com/jup-ag/jupiter-lend/blob/33a22cf7a5bfdd32ab1712dda4adfbeb9b348ad9/target/idl/liquidity.json>.
//! The matching Fluid implementation at commit
//! `626b177f235224bc5d074d39439cd2558f542886` gives the rate units:
//! <https://github.com/Instadapp/fluid-solana-programs/blob/626b177f235224bc5d074d39439cd2558f542886/programs/liquidity/src/state/token_reserve.rs>.
//!
//! Events describe execution at their log position. `LogOperate` does not emit
//! the new borrow rate, and an exchange-price event is not complete account
//! state. The caller must check transaction success and bank finality. This
//! module does not convert events to current yield observations or invent APY.

use anyhow::{anyhow, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

pub const JUPITER_LIQUIDITY_PROGRAM: &str = "jupeiUmn818Jg1ekPURTpr4mFo29p46vygyykFJ3wZC";

const OPERATE: [u8; 8] = [180, 8, 81, 71, 19, 132, 173, 8];
const UPDATE_EXCHANGE_PRICES: [u8; 8] = [190, 194, 69, 204, 30, 86, 181, 163];
const MAX_LOGS: usize = 4_096;
const MAX_LOG_BYTES: usize = 1_048_576;
const MAX_LINE_BYTES: usize = 16_384;
const MAX_EVENT_BYTES: usize = 4_096;
const MAX_EVENTS: usize = 512;
const MAX_DEPTH: usize = 64;

/// Raw amounts and exchange prices use decimal strings to retain all bits in
/// JSON. `borrow_apr` is an annual fraction, not a percentage or an APY.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JupiterEvent {
    Operate {
        /// Zero-based position in the original transaction log list.
        log_index: usize,
        user: String,
        token: String,
        supply_amount_raw: String,
        borrow_amount_raw: String,
        withdraw_to: String,
        borrow_to: String,
        supply_exchange_price_raw: String,
        borrow_exchange_price_raw: String,
    },
    UpdateExchangePrices {
        /// Zero-based position in the original transaction log list.
        log_index: usize,
        token: String,
        supply_exchange_price_raw: String,
        borrow_exchange_price_raw: String,
        borrow_rate_bps: u16,
        borrow_apr: f64,
        utilization_bps: u16,
    },
}

impl JupiterEvent {
    pub fn log_index(&self) -> usize {
        match self {
            Self::Operate { log_index, .. } | Self::UpdateExchangePrices { log_index, .. } => {
                *log_index
            }
        }
    }

    pub fn token(&self) -> &str {
        match self {
            Self::Operate { token, .. } | Self::UpdateExchangePrices { token, .. } => token,
        }
    }
}

struct Frame {
    program: String,
    events: Vec<JupiterEvent>,
}

/// Parse events only while the exact liquidity program is the active runtime
/// invocation. A matching discriminator from a child or another program has no
/// authority. Events remain in their frame until success; failed frames and
/// their successful children are discarded together.
///
/// Any malformed runtime stack or truncated recognized event rejects the full
/// result. The caller must not store partial results after this function fails.
pub fn parse_jupiter_logs(logs: &[String]) -> Result<Vec<JupiterEvent>> {
    ensure!(logs.len() <= MAX_LOGS, "transaction has too many log lines");
    let mut total_bytes = 0usize;
    for line in logs {
        ensure!(
            line.len() <= MAX_LINE_BYTES,
            "transaction log line is too large"
        );
        total_bytes = total_bytes
            .checked_add(line.len())
            .ok_or_else(|| anyhow!("transaction log size overflow"))?;
    }
    ensure!(
        total_bytes <= MAX_LOG_BYTES,
        "transaction logs are too large"
    );

    let mut stack: Vec<Frame> = Vec::new();
    let mut completed = Vec::new();
    let mut parsed_events = 0usize;
    for (log_index, line) in logs.iter().enumerate() {
        ensure!(
            line != "Log truncated" && line != "Program log: Log truncated",
            "transaction logs were truncated"
        );
        if let Some((program, depth)) = invocation(line)? {
            ensure!(depth == stack.len() + 1, "invalid program invocation depth");
            ensure!(depth <= MAX_DEPTH, "program invocation depth is too large");
            stack.push(Frame {
                program: program.to_owned(),
                events: Vec::new(),
            });
            continue;
        }
        if let Some((program, succeeded)) = completion(line)? {
            let frame = stack
                .pop()
                .context("program completion has no invocation")?;
            ensure!(
                frame.program == program,
                "program completion does not match invocation"
            );
            if succeeded {
                if let Some(parent) = stack.last_mut() {
                    parent.events.extend(frame.events);
                } else {
                    completed.extend(frame.events);
                }
            }
            continue;
        }
        let Some(encoded) = line.strip_prefix("Program data: ") else {
            continue;
        };
        // Never decode or accept a child program's event as a Jupiter event.
        if stack
            .last()
            .is_some_and(|frame| frame.program != JUPITER_LIQUIDITY_PROGRAM)
        {
            continue;
        }
        ensure!(
            encoded.len() <= MAX_EVENT_BYTES.div_ceil(3) * 4,
            "program event is too large"
        );
        let bytes = STANDARD
            .decode(encoded)
            .context("invalid program event base64")?;
        ensure!(bytes.len() <= MAX_EVENT_BYTES, "program event is too large");
        let Some(event) = decode_event(&bytes, log_index)? else {
            continue;
        };
        let frame = stack
            .last_mut()
            .context("Jupiter event has no program invocation")?;
        parsed_events += 1;
        ensure!(
            parsed_events <= MAX_EVENTS,
            "transaction has too many Jupiter events"
        );
        frame.events.push(event);
    }
    ensure!(
        stack.is_empty(),
        "transaction program invocation logs are incomplete"
    );
    Ok(completed)
}

fn valid_program(program: &str) -> Result<()> {
    let mut bytes = [0u8; 32];
    ensure!(
        bs58::decode(program)
            .onto(&mut bytes)
            .context("invalid runtime program address")?
            == 32,
        "invalid runtime program address length"
    );
    Ok(())
}

fn invocation(line: &str) -> Result<Option<(&str, usize)>> {
    let Some(rest) = line.strip_prefix("Program ") else {
        return Ok(None);
    };
    if rest.starts_with("log:") || rest.starts_with("data:") || rest.starts_with("return:") {
        return Ok(None);
    }
    let Some((program, depth)) = rest.split_once(" invoke [") else {
        return Ok(None);
    };
    valid_program(program)?;
    let depth = depth
        .strip_suffix(']')
        .context("invalid program invocation log")?;
    ensure!(
        !depth.is_empty() && depth.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid program invocation depth"
    );
    Ok(Some((
        program,
        depth.parse().context("invalid program invocation depth")?,
    )))
}

fn completion(line: &str) -> Result<Option<(&str, bool)>> {
    let Some(rest) = line.strip_prefix("Program ") else {
        return Ok(None);
    };
    if let Some(program) = rest.strip_suffix(" success") {
        // User messages start with `Program log:` and cannot complete frames.
        if program.starts_with("log:") {
            return Ok(None);
        }
        valid_program(program)?;
        return Ok(Some((program, true)));
    }
    if let Some((program, _)) = rest.split_once(" failed: ") {
        if program.starts_with("log:") {
            return Ok(None);
        }
        valid_program(program)?;
        return Ok(Some((program, false)));
    }
    Ok(None)
}

fn decode_event(bytes: &[u8], log_index: usize) -> Result<Option<JupiterEvent>> {
    let Some(discriminator) = bytes.get(..8) else {
        ensure!(
            bytes.is_empty()
                || (!OPERATE.starts_with(bytes) && !UPDATE_EXCHANGE_PRICES.starts_with(bytes)),
            "truncated Jupiter event discriminator"
        );
        return Ok(None);
    };
    let key = |at| bs58::encode(&bytes[at..at + 32]).into_string();
    let wide = |at| u128::from_le_bytes(bytes[at..at + 16].try_into().unwrap());
    let signed = |at| i128::from_le_bytes(bytes[at..at + 16].try_into().unwrap());
    let narrow = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let small = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    if discriminator == OPERATE {
        ensure!(bytes.len() == 184, "invalid Jupiter LogOperate length");
        return Ok(Some(JupiterEvent::Operate {
            log_index,
            user: key(8),
            token: key(40),
            supply_amount_raw: signed(72).to_string(),
            borrow_amount_raw: signed(88).to_string(),
            withdraw_to: key(104),
            borrow_to: key(136),
            supply_exchange_price_raw: narrow(168).to_string(),
            borrow_exchange_price_raw: narrow(176).to_string(),
        }));
    }
    if discriminator == UPDATE_EXCHANGE_PRICES {
        ensure!(
            bytes.len() == 76,
            "invalid Jupiter LogUpdateExchangePrices length"
        );
        let borrow_rate_bps = small(72);
        return Ok(Some(JupiterEvent::UpdateExchangePrices {
            log_index,
            token: key(8),
            supply_exchange_price_raw: wide(40).to_string(),
            borrow_exchange_price_raw: wide(56).to_string(),
            borrow_rate_bps,
            borrow_apr: f64::from(borrow_rate_bps) / 10_000.0,
            utilization_bps: small(74),
        }));
    }
    Ok(None)
}
