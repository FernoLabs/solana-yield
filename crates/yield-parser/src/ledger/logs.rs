use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;

use crate::{jupiter_ledger, key, LedgerMeasurement};

use super::LogStatus;

const MAX_LOGS: usize = 4_096;
const MAX_LOG_BYTES: usize = 1_048_576;
const MAX_LINE_BYTES: usize = 16_384;
const MAX_EVENTS: usize = 512;
const MAX_DEPTH: usize = 64;

pub(super) struct Event {
    pub log_index: usize,
    pub measurement: LedgerMeasurement,
}

#[derive(Default)]
pub(super) struct Trace {
    pub events: Vec<Event>,
    pub earn_invocations: usize,
    pub earn_top_level: bool,
}

struct Frame<'a> {
    program: &'a str,
    events: Vec<Event>,
}

pub(super) fn parse(value: Option<&Value>) -> Result<Trace, LogStatus> {
    use LogStatus::{Invalid, Truncated, Unavailable};
    let lines = match value {
        None | Some(Value::Null) => return Err(Unavailable),
        Some(value) => value.as_array().ok_or(Invalid)?,
    };
    if lines.is_empty() {
        return Err(Unavailable);
    }
    if lines.len() > MAX_LOGS {
        return Err(Invalid);
    }
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut trace = Trace::default();
    let mut total_bytes = 0;
    let mut event_count = 0;
    for (log_index, line) in lines.iter().enumerate() {
        let line = line.as_str().ok_or(Invalid)?;
        total_bytes += line.len();
        if line.len() > MAX_LINE_BYTES || total_bytes > MAX_LOG_BYTES {
            return Err(Invalid);
        }
        if matches!(line, "Log truncated" | "Program log: Log truncated") {
            return Err(Truncated);
        }
        let Some(program_line) = line.strip_prefix("Program ") else {
            continue;
        };
        if let Some(encoded) = program_line.strip_prefix("data: ") {
            let frame = stack.last_mut().ok_or(Invalid)?;
            if matches!(
                frame.program,
                jupiter_ledger::EARN_PROGRAM | jupiter_ledger::LIQUIDITY_PROGRAM
            ) {
                let bytes = STANDARD.decode(encoded).map_err(|_| Invalid)?;
                if let Some(measurement) =
                    jupiter_ledger::decode(frame.program, &bytes).map_err(|_| Invalid)?
                {
                    event_count += 1;
                    if event_count > MAX_EVENTS {
                        return Err(Invalid);
                    }
                    frame.events.push(Event {
                        log_index,
                        measurement,
                    });
                }
            }
            continue;
        }
        if program_line.starts_with("log:") || program_line.starts_with("return:") {
            continue;
        }
        if let Some((program, depth)) = program_line.split_once(" invoke [") {
            key(program).map_err(|_| Invalid)?;
            let depth = depth.strip_suffix(']').ok_or(Invalid)?;
            if depth.is_empty() || !depth.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Invalid);
            }
            let depth = depth.parse::<usize>().map_err(|_| Invalid)?;
            if depth != stack.len() + 1 || depth > MAX_DEPTH {
                return Err(Invalid);
            }
            if program == jupiter_ledger::EARN_PROGRAM {
                trace.earn_invocations += 1;
                trace.earn_top_level |= depth == 1;
            }
            stack.push(Frame {
                program,
                events: Vec::new(),
            });
            continue;
        }
        let completion = program_line
            .strip_suffix(" success")
            .map(|program| (program, true))
            .or_else(|| {
                program_line
                    .split_once(" failed: ")
                    .map(|(program, _)| (program, false))
            });
        if let Some((program, succeeded)) = completion {
            let frame = stack.pop().ok_or(Invalid)?;
            if frame.program != program {
                return Err(Invalid);
            }
            if succeeded {
                if let Some(parent) = stack.last_mut() {
                    parent.events.extend(frame.events);
                } else {
                    trace.events.extend(frame.events);
                }
            } else if stack.is_empty() {
                // A successful transaction cannot have a failed outer call.
                return Err(Invalid);
            }
        }
    }
    if !stack.is_empty() {
        return Err(Invalid);
    }
    Ok(trace)
}
