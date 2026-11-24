//! Bounded application records for one pinned Blockzilla capture.
//!
//! No raw frame or account bytes are retained. Selected normalized transactions,
//! instruction facts, events, bank status and the application cursor commit together.
use anyhow::{ensure, Context, Result};
use blockzilla_bank_client::{BankEvent, BankIdentity, Frame, SourceIdentity};
use blockzilla_bank_stream_v2::chain::{BankState, Chain};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};
use yield_indexer::stream_parser::JupiterEvent;

use crate::continuity::{self, ContinuityReport};

const MAX_BANKS: i64 = 10_000;
const MAX_EVENTS: i64 = 100_000;
const MAX_CANDIDATES: i64 = 100_000;
const MAX_INSTRUCTIONS: i64 = 100_000;
const MAX_FRAMES: i64 = 50_000;
const MAX_PARSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_CANDIDATE_BYTES: usize = 1024 * 1024;

#[derive(Serialize)]
pub struct ParsedCandidate {
    pub signature: String,
    pub transaction_index: usize,
    pub execution_status: String,
    pub log_status: String,
    pub log_reason: Option<String>,
    pub parser_version: String,
    pub transaction: Value,
}

#[derive(Serialize)]
pub struct ParsedInstruction {
    pub signature: String,
    pub transaction_index: usize,
    pub outer_instruction_index: usize,
    pub inner_instruction_index: Option<usize>,
    pub stack_height: Option<u32>,
    pub program: String,
    pub name: String,
    pub execution_status: String,
    pub data_base58: String,
    pub accounts: Vec<String>,
    pub arguments: Value,
    pub decode_status: String,
    pub decode_reason: Option<String>,
}

#[derive(Serialize)]
pub struct ParsedEvent {
    pub signature: String,
    pub transaction_index: usize,
    pub event: JupiterEvent,
}

#[derive(Serialize)]
pub struct ParserRejection {
    pub signature: String,
    pub transaction_index: usize,
    pub reason: String,
}

#[derive(Serialize)]
pub struct ParsedBlock {
    pub block_time: Option<i64>,
    pub transactions_scanned: usize,
    pub transactions_selected: usize,
    pub transactions_succeeded: usize,
    pub transactions_failed: usize,
    pub transactions_without_metadata: usize,
    pub parser_rejections: usize,
    pub parser_rejection_details: Vec<ParserRejection>,
    pub candidates: Vec<ParsedCandidate>,
    pub instructions: Vec<ParsedInstruction>,
    pub events: Vec<ParsedEvent>,
}

pub struct StreamStore {
    db: Connection,
    source: SourceIdentity,
}

fn meta(db: &Connection, key: &str) -> Result<Option<String>> {
    Ok(db
        .query_row("SELECT value FROM stream_meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

fn put_meta(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT INTO stream_meta(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn bank_key(bank: &BankIdentity) -> String {
    format!("{}:{}", bank.slot, bank.bank_id)
}

fn admit(frame: &Frame, source: &SourceIdentity) -> Result<()> {
    let checked = blockzilla_bank_client::decode(frame.wire.clone())?;
    ensure!(
        checked.source == *source
            && frame.source == *source
            && checked.sha256 == frame.sha256
            && checked.cursor == frame.cursor
            && serde_json::to_value(&checked.envelope)? == serde_json::to_value(&frame.envelope)?,
        "stream frame does not match its admitted bytes or pinned source"
    );
    Ok(())
}

fn parsed_value(parsed: &ParsedBlock) -> Result<Value> {
    ensure!(
        parsed.transactions_selected <= parsed.transactions_scanned
            && parsed.transactions_scanned <= 65_536
            && parsed.parser_rejections <= parsed.transactions_selected
            && parsed.parser_rejection_details.len() == parsed.parser_rejections
            && parsed
                .transactions_succeeded
                .checked_add(parsed.transactions_failed)
                .and_then(|count| count.checked_add(parsed.transactions_without_metadata))
                == Some(parsed.transactions_selected)
            && parsed.events.len() <= MAX_EVENTS as usize
            && parsed.candidates.len() <= parsed.transactions_selected
            && parsed.candidates.len() <= MAX_CANDIDATES as usize
            && parsed.instructions.len() <= MAX_INSTRUCTIONS as usize,
        "invalid parsed transaction counts"
    );
    ensure!(
        parsed.block_time.is_none_or(|time| time > 0),
        "invalid parsed block time"
    );
    let mut events = Vec::with_capacity(parsed.events.len());
    let mut total_bytes = 0usize;
    let mut candidates = Vec::with_capacity(parsed.candidates.len());
    let mut candidate_positions = BTreeMap::new();
    for candidate in &parsed.candidates {
        ensure!(
            bounded_ascii(&candidate.signature, 128)
                && candidate.transaction_index < parsed.transactions_scanned
                && matches!(
                    candidate.execution_status.as_str(),
                    "succeeded" | "failed" | "unknown"
                )
                && matches!(
                    candidate.log_status.as_str(),
                    "complete" | "unavailable" | "truncated" | "invalid"
                )
                && bounded_reason(candidate.log_reason.as_deref())
                && bounded_ascii(&candidate.parser_version, 128)
                && candidate.transaction.is_object(),
            "invalid parsed candidate"
        );
        ensure!(
            candidate_positions
                .insert(candidate.transaction_index, candidate)
                .is_none(),
            "duplicate parsed candidate position"
        );
        let value = serde_json::to_value(candidate)?;
        let bytes = serde_json::to_vec(&value)?.len();
        ensure!(
            bytes <= MAX_CANDIDATE_BYTES,
            "parsed candidate exceeds size limit"
        );
        account_bytes(&mut total_bytes, bytes)?;
        candidates.push(value);
    }
    let mut instructions = Vec::with_capacity(parsed.instructions.len());
    let mut instruction_positions = BTreeSet::new();
    for instruction in &parsed.instructions {
        let candidate = candidate_positions.get(&instruction.transaction_index);
        ensure!(
            candidate.is_some_and(|candidate| candidate.signature == instruction.signature)
                && candidate.is_some_and(|candidate| match candidate.execution_status.as_str() {
                    "failed" => instruction.execution_status == "failed_transaction",
                    "unknown" => instruction.execution_status == "unknown_transaction",
                    "succeeded" if instruction.inner_instruction_index.is_some() =>
                        instruction.execution_status == "attempted_cpi_unproven",
                    "succeeded" => instruction.execution_status == "committed_top_level",
                    _ => false,
                })
                && instruction.outer_instruction_index < 65_536
                && instruction
                    .inner_instruction_index
                    .is_none_or(|index| index < 65_536)
                && instruction
                    .stack_height
                    .is_none_or(|height| height > 0 && height <= 256)
                && bounded_ascii(&instruction.program, 64)
                && bounded_ascii(&instruction.name, 128)
                && matches!(
                    instruction.execution_status.as_str(),
                    "committed_top_level"
                        | "attempted_cpi_unproven"
                        | "failed_transaction"
                        | "unknown_transaction"
                )
                && instruction.data_base58.len() <= 65_536
                && instruction.data_base58.is_ascii()
                && instruction.accounts.len() <= 256
                && instruction
                    .accounts
                    .iter()
                    .all(|account| bounded_ascii(account, 64))
                && matches!(
                    instruction.decode_status.as_str(),
                    "decoded" | "unsupported" | "invalid"
                )
                && bounded_reason(instruction.decode_reason.as_deref()),
            "invalid parsed instruction"
        );
        ensure!(
            instruction_positions.insert((
                instruction.transaction_index,
                instruction.outer_instruction_index,
                instruction.inner_instruction_index
            )),
            "duplicate parsed instruction position"
        );
        let value = serde_json::to_value(instruction)?;
        let bytes = serde_json::to_vec(&value)?.len();
        ensure!(
            bytes <= MAX_CANDIDATE_BYTES,
            "parsed instruction exceeds size limit"
        );
        account_bytes(&mut total_bytes, bytes)?;
        instructions.push(value);
    }
    for rejection in &parsed.parser_rejection_details {
        ensure!(
            !rejection.signature.is_empty()
                && rejection.signature.len() <= 128
                && rejection.signature.is_ascii()
                && rejection.transaction_index < parsed.transactions_scanned
                && !rejection.reason.is_empty()
                && rejection.reason.len() <= 512,
            "invalid parser rejection diagnostic"
        );
        total_bytes = total_bytes
            .checked_add(rejection.signature.len() + rejection.reason.len() + 128)
            .context("parsed block size overflow")?;
        ensure!(
            total_bytes <= MAX_PARSED_BYTES - 1_024,
            "parsed block exceeds size limit"
        );
    }
    for event in &parsed.events {
        ensure!(
            !event.signature.is_empty()
                && event.signature.len() <= 128
                && event.signature.is_ascii()
                && event.transaction_index < parsed.transactions_scanned,
            "invalid parsed transaction identity"
        );
        let value = serde_json::to_value(&event.event)?;
        let event_bytes = serde_json::to_vec(&value)?.len();
        ensure!(event_bytes <= 16_384, "parsed event exceeds size limit");
        total_bytes = total_bytes
            .checked_add(event_bytes + event.signature.len() + 128)
            .context("parsed block size overflow")?;
        ensure!(
            total_bytes <= MAX_PARSED_BYTES - 1_024,
            "parsed block exceeds size limit"
        );
        events.push(json!({
            "signature": event.signature,
            "transaction_index": event.transaction_index,
            "event": value,
        }));
    }
    let value = json!({
        "block_time": parsed.block_time,
        "transactions_scanned": parsed.transactions_scanned,
        "transactions_selected": parsed.transactions_selected,
        "transactions_succeeded": parsed.transactions_succeeded,
        "transactions_failed": parsed.transactions_failed,
        "transactions_without_metadata": parsed.transactions_without_metadata,
        "parser_rejections": parsed.parser_rejections,
        "parser_rejection_details": parsed.parser_rejection_details,
        "candidates": candidates,
        "instructions": instructions,
        "events": events,
    });
    ensure!(
        serde_json::to_vec(&value)?.len() <= MAX_PARSED_BYTES,
        "parsed block exceeds size limit"
    );
    Ok(value)
}

fn bounded_ascii(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.is_ascii()
        && !value.contains(['\r', '\n', '\0'])
}

fn bounded_reason(value: Option<&str>) -> bool {
    value.is_none_or(|reason| !reason.is_empty() && reason.len() <= 512)
}

fn account_bytes(total: &mut usize, bytes: usize) -> Result<()> {
    *total = bytes
        .checked_add(128)
        .and_then(|bytes| total.checked_add(bytes))
        .context("parsed block size overflow")?;
    ensure!(
        *total <= MAX_PARSED_BYTES - 1_024,
        "parsed block exceeds size limit"
    );
    Ok(())
}

fn parsed_core(mut parsed: Value) -> Result<Value> {
    // Diagnostics were added after schema 1 captures already existed. They do
    // not change admitted events or counts. Preserve the original saved record
    // and compare all other fields when a dependency is decoded after restart.
    parsed
        .as_object_mut()
        .context("invalid saved parsed block JSON")?
        .remove("parser_rejection_details");
    Ok(parsed)
}

impl StreamStore {
    pub fn open(path: &Path, source: &SourceIdentity, origin: &str) -> Result<Self> {
        source.validate()?;
        ensure!(
            !origin.is_empty() && origin.len() <= 2_048 && !origin.contains(['\r', '\n']),
            "invalid stream origin"
        );
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).context("create stream database directory")?;
        }
        let mut db = Connection::open(path).context("open stream application database")?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS stream_meta (
                 key TEXT PRIMARY KEY, value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS stream_banks (
                 bank_key TEXT PRIMARY KEY,
                 identity_json TEXT NOT NULL,
                 payload_frame_sha256 TEXT NOT NULL UNIQUE,
                 payload_sha256 TEXT NOT NULL,
                 block_height INTEGER NOT NULL,
                 payload_sequence INTEGER NOT NULL,
                 payload_event_json TEXT NOT NULL,
                 application_eligible INTEGER NOT NULL CHECK(application_eligible IN (0,1)),
                 continuity_contiguous INTEGER NOT NULL DEFAULT 0 CHECK(continuity_contiguous IN (0,1)),
                 status TEXT NOT NULL CHECK(status IN ('processed','confirmed','finalized','invalidated')),
                 parsed_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS stream_frames (
                 cursor TEXT PRIMARY KEY, sequence INTEGER NOT NULL UNIQUE,
                 sha256 TEXT NOT NULL UNIQUE, bank_key TEXT NOT NULL,
                 kind TEXT NOT NULL, event_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS stream_parser_events (
                 payload_frame_sha256 TEXT NOT NULL,
                 event_index INTEGER NOT NULL,
                 bank_key TEXT NOT NULL REFERENCES stream_banks(bank_key),
                 status TEXT NOT NULL CHECK(status IN ('processed','confirmed','finalized','invalidated')),
                 signature TEXT NOT NULL, transaction_index INTEGER NOT NULL,
                 event_json TEXT NOT NULL,
                 PRIMARY KEY(payload_frame_sha256,event_index)
             );
             CREATE INDEX IF NOT EXISTS stream_parser_bank ON stream_parser_events(bank_key);
             CREATE TABLE IF NOT EXISTS stream_candidates (
                 payload_frame_sha256 TEXT NOT NULL,
                 transaction_index INTEGER NOT NULL,
                 bank_key TEXT NOT NULL REFERENCES stream_banks(bank_key),
                 status TEXT NOT NULL CHECK(status IN ('processed','confirmed','finalized','invalidated')),
                 signature TEXT NOT NULL,
                 execution_status TEXT NOT NULL CHECK(execution_status IN ('succeeded','failed','unknown')),
                 log_status TEXT NOT NULL CHECK(log_status IN ('complete','unavailable','truncated','invalid')),
                 log_reason TEXT,
                 parser_version TEXT NOT NULL,
                 transaction_json TEXT NOT NULL,
                 PRIMARY KEY(payload_frame_sha256,transaction_index)
             );
             CREATE INDEX IF NOT EXISTS stream_candidates_bank ON stream_candidates(bank_key);
             CREATE TABLE IF NOT EXISTS stream_instructions (
                 payload_frame_sha256 TEXT NOT NULL,
                 transaction_index INTEGER NOT NULL,
                 outer_instruction_index INTEGER NOT NULL,
                 inner_instruction_index INTEGER NOT NULL,
                 bank_key TEXT NOT NULL REFERENCES stream_banks(bank_key),
                 status TEXT NOT NULL CHECK(status IN ('processed','confirmed','finalized','invalidated')),
                 signature TEXT NOT NULL,
                 stack_height INTEGER,
                 program TEXT NOT NULL,
                 name TEXT NOT NULL,
                 execution_status TEXT NOT NULL CHECK(execution_status IN ('committed_top_level','attempted_cpi_unproven','failed_transaction','unknown_transaction')),
                 data_base58 TEXT NOT NULL,
                 accounts_json TEXT NOT NULL,
                 arguments_json TEXT NOT NULL,
                 decode_status TEXT NOT NULL CHECK(decode_status IN ('decoded','unsupported','invalid')),
                 decode_reason TEXT,
                 PRIMARY KEY(payload_frame_sha256,transaction_index,outer_instruction_index,inner_instruction_index),
                 FOREIGN KEY(payload_frame_sha256,transaction_index) REFERENCES stream_candidates(payload_frame_sha256,transaction_index)
             );
             CREATE INDEX IF NOT EXISTS stream_instructions_bank ON stream_instructions(bank_key);",
        )?;
        let has_continuity_column = {
            let mut statement = db.prepare("PRAGMA table_info(stream_banks)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            columns
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .any(|column| column == "continuity_contiguous")
        };
        if !has_continuity_column {
            db.execute_batch("ALTER TABLE stream_banks ADD COLUMN continuity_contiguous INTEGER NOT NULL DEFAULT 0 CHECK(continuity_contiguous IN (0,1));")?;
        }
        let tx = db.transaction()?;
        let encoded_source = serde_json::to_string(source)?;
        match (meta(&tx, "source")?, meta(&tx, "origin")?) {
            (Some(previous), Some(previous_origin)) => {
                ensure!(
                    serde_json::from_str::<SourceIdentity>(&previous)? == *source
                        && previous_origin == origin,
                    "saved stream source or origin changed"
                );
            }
            (None, None) => {
                let records: i64 = tx.query_row(
                    "SELECT (SELECT COUNT(*) FROM stream_banks)
                          + (SELECT COUNT(*) FROM stream_frames)
                          + (SELECT COUNT(*) FROM stream_parser_events)
                          + (SELECT COUNT(*) FROM stream_candidates)
                          + (SELECT COUNT(*) FROM stream_instructions)",
                    [],
                    |row| row.get(0),
                )?;
                ensure!(
                    records == 0 && meta(&tx, "cursor")?.is_none(),
                    "unidentified saved stream data"
                );
                put_meta(&tx, "source", &encoded_source)?;
                put_meta(&tx, "origin", origin)?;
                put_meta(&tx, "schema_version", "1")?;
            }
            _ => anyhow::bail!("incomplete saved stream source pin"),
        }
        ensure!(
            meta(&tx, "schema_version")?.as_deref() == Some("1"),
            "unsupported stream database schema"
        );
        if let Some(cursor) = meta(&tx, "cursor")? {
            ensure!(
                blockzilla_bank_client::parse_cursor(&cursor)?.0 == source.session_id,
                "saved cursor source changed"
            );
            if saved_continuity(&tx)?.is_none() {
                refresh_continuity(&tx, source)?;
            }
            if let Some(saved) = saved_continuity(&tx)? {
                let input = continuity_input(&tx, source)?
                    .context("saved continuity has no capture boundary")?;
                ensure_same_capture(&saved.capture, &input.capture)?;
                let expected = continuity::validate_saved_report(
                    &input.chain,
                    &input.capture,
                    &saved,
                    &input.root_cursors,
                    &input.eligible,
                )?;
                let retained = {
                    let mut statement = tx.prepare("SELECT payload_frame_sha256,application_eligible,continuity_contiguous FROM stream_banks WHERE continuity_contiguous<>0")?;
                    let rows = statement.query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, bool>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?;
                    let mut retained = BTreeSet::new();
                    for row in rows {
                        let (digest, eligible, contiguous) = row?;
                        ensure!(
                            eligible && contiguous == 1 && input.eligible.contains(&digest),
                            "saved continuity flag marks an ineligible bank"
                        );
                        retained.insert(digest);
                    }
                    retained
                };
                ensure!(
                    retained == expected,
                    "saved continuity coverage flags changed"
                );
            }
        }
        tx.commit()?;
        Ok(Self {
            db,
            source: source.clone(),
        })
    }

    pub fn cursor(&self) -> Result<Option<String>> {
        meta(&self.db, "cursor")
    }

    pub fn initialize(&mut self, after: &str, payload: &Frame, parsed: &ParsedBlock) -> Result<()> {
        admit(payload, &self.source)?;
        ensure!(
            payload.envelope.event.kind == "block",
            "capture requires a processed payload"
        );
        ensure!(
            blockzilla_bank_client::parse_cursor(after)?.0 == self.source.session_id,
            "capture cursor source changed"
        );
        let tx = self.db.transaction()?;
        if let Some(previous) = meta(&tx, "cursor")? {
            ensure!(
                previous == after,
                "saved stream cursor changed during bootstrap"
            );
            ensure!(
                meta(&tx, "capture_cursor")?.is_some(),
                "saved capture boundary missing"
            );
            return Ok(());
        }
        ensure!(
            after == payload.cursor,
            "first capture must start at the exact payload cursor"
        );
        put_meta(&tx, "capture_cursor", after)?;
        put_meta(
            &tx,
            "capture_slot",
            &payload.envelope.event.bank.slot.to_string(),
        )?;
        insert_payload(&tx, payload, parsed, payload.envelope.event.sequence)?;
        insert_frame(&tx, payload)?;
        refresh_continuity(&tx, &self.source)?;
        put_meta(&tx, "cursor", after)?;
        tx.commit()?;
        Ok(())
    }

    pub fn apply(
        &mut self,
        frame: &Frame,
        dependency: Option<(&Frame, &ParsedBlock)>,
        parsed: Option<&ParsedBlock>,
    ) -> Result<()> {
        admit(frame, &self.source)?;
        if let Some((payload, _)) = dependency {
            admit(payload, &self.source)?;
            ensure!(
                frame.envelope.event.kind != "block",
                "processed frame cannot have a control dependency"
            );
            frame.envelope.event.matches_payload(payload)?;
            ensure!(
                payload.envelope.event.sequence < frame.envelope.event.sequence,
                "control dependency is not older than control"
            );
        }
        let tx = self.db.transaction()?;
        let cursor = meta(&tx, "cursor")?.context("stream capture is not initialized")?;
        let saved_sequence = blockzilla_bank_client::parse_cursor(&cursor)?.1;
        let event = &frame.envelope.event;
        if event.sequence <= saved_sequence {
            let previous: Option<(String, String)> = tx
                .query_row(
                    "SELECT cursor,event_json FROM stream_frames WHERE sequence=?1",
                    [event.sequence as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            ensure!(
                previous == Some((frame.cursor.clone(), serde_json::to_string(event)?)),
                "duplicate stream sequence has different or absent evidence"
            );
            return Ok(());
        }
        ensure!(
            event.sequence == saved_sequence + 1,
            "stream application sequence gap"
        );
        let boundary = meta(&tx, "capture_cursor")?.context("stream capture boundary missing")?;
        let capture_sequence = blockzilla_bank_client::parse_cursor(&boundary)?.1;
        if event.kind == "block" {
            ensure!(
                dependency.is_none(),
                "processed frame cannot have a dependency"
            );
            insert_payload(
                &tx,
                frame,
                parsed.context("processed frame has no parsed block")?,
                capture_sequence,
            )?;
        } else {
            ensure!(
                parsed.is_none(),
                "control frame cannot contain parsed transactions"
            );
            if let Some((payload, parsed_payload)) = dependency {
                insert_payload(&tx, payload, parsed_payload, capture_sequence)?;
            }
            apply_control(&tx, event)?;
        }
        insert_frame(&tx, frame)?;
        let saved_continuity = saved_continuity(&tx)?;
        let payload_height = if event.kind == "block" {
            event.block_height
        } else {
            dependency.and_then(|(payload, _)| payload.envelope.event.block_height)
        };
        let affects_verified_prefix = saved_continuity
            .as_ref()
            .and_then(|report| report.verified_root.as_ref())
            .is_some_and(|root| payload_height.is_some_and(|height| height <= root.block_height));
        let refresh = matches!(event.kind.as_str(), "root" | "invalidate")
            || ((event.kind == "block" || dependency.is_some())
                && (affects_verified_prefix
                    || saved_continuity.is_none_or(|report| report.status != "contiguous")));
        if refresh {
            refresh_continuity(&tx, &self.source)?;
        }
        put_meta(&tx, "cursor", &frame.cursor)?;
        tx.commit()?;
        Ok(())
    }

    pub fn summary(&self) -> Result<Value> {
        let mut summary = json!({
            "cursor": self.cursor()?,
            "capture_cursor": meta(&self.db, "capture_cursor")?,
            "capture_slot": meta(&self.db, "capture_slot")?.map(|slot| slot.parse::<u64>()).transpose()?,
            "raw_payload_bytes_retained": 0,
            "limits": {"banks": MAX_BANKS, "events": MAX_EVENTS, "candidates": MAX_CANDIDATES, "instructions": MAX_INSTRUCTIONS, "frames": MAX_FRAMES},
            "coverage": "Exact captured banks only; no complete historical or account-state coverage",
            "candidate_usage": "Transaction and instruction evidence only; not complete account state or current yield",
            "continuity": saved_continuity(&self.db)?,
            "continuity_covered_banks": self.db.query_row("SELECT COUNT(*) FROM stream_banks WHERE application_eligible=1 AND continuity_contiguous=1", [], |row| row.get::<_, i64>(0))?,
        });
        for (name, table) in [
            ("banks", "stream_banks"),
            ("frames", "stream_frames"),
            ("events", "stream_parser_events"),
            ("candidates", "stream_candidates"),
            ("instructions", "stream_instructions"),
        ] {
            let count: i64 =
                self.db
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
            summary[name] = json!(count);
        }
        for field in [
            "transactions_scanned",
            "transactions_selected",
            "transactions_succeeded",
            "transactions_failed",
            "transactions_without_metadata",
            "parser_rejections",
        ] {
            let count: i64 = self.db.query_row(
                &format!("SELECT COALESCE(SUM(json_extract(parsed_json,'$.{field}')),0) FROM stream_banks WHERE application_eligible=1"),
                [], |row| row.get(0),
            )?;
            summary[field] = json!(count);
        }
        summary["log_enrichment_failures"] = summary["parser_rejections"].clone();
        summary["parser_rejections_scope"] =
            json!("Legacy log-enrichment counter; candidates and instructions are retained");
        for (name, table, column) in [
            ("bank_statuses", "stream_banks", "status"),
            ("event_statuses", "stream_parser_events", "status"),
            ("candidate_statuses", "stream_candidates", "status"),
            ("instruction_statuses", "stream_instructions", "status"),
            (
                "candidate_execution_statuses",
                "stream_candidates",
                "execution_status",
            ),
            ("candidate_log_statuses", "stream_candidates", "log_status"),
            (
                "instruction_execution_statuses",
                "stream_instructions",
                "execution_status",
            ),
            (
                "instruction_decode_statuses",
                "stream_instructions",
                "decode_status",
            ),
            ("control_counts", "stream_frames", "kind"),
        ] {
            let mut counts = serde_json::Map::new();
            let mut statement = self.db.prepare(&format!(
                "SELECT {column},COUNT(*) FROM {table} GROUP BY {column}"
            ))?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (kind, count) = row?;
                counts.insert(kind, json!(count));
            }
            summary[name] = Value::Object(counts);
        }
        Ok(summary)
    }
}

struct ContinuityInput {
    chain: Chain,
    capture: BankState,
    root_cursors: BTreeMap<String, String>,
    eligible: BTreeSet<String>,
}

fn saved_continuity(db: &Connection) -> Result<Option<ContinuityReport>> {
    meta(db, "stream_continuity")?
        .map(|encoded| {
            ensure!(
                encoded.len() <= MAX_PARSED_BYTES,
                "saved continuity report exceeds size limit"
            );
            serde_json::from_str(&encoded).context("invalid saved continuity report")
        })
        .transpose()
}

fn ensure_same_capture(previous: &BankState, capture: &BankState) -> Result<()> {
    ensure!(
        previous.bank == capture.bank
            && previous.block_height == capture.block_height
            && previous.payload_sha256 == capture.payload_sha256
            && previous.payload_frame_sha256 == capture.payload_frame_sha256,
        "saved continuity capture boundary changed"
    );
    Ok(())
}

fn continuity_input(
    tx: &Transaction<'_>,
    source: &SourceIdentity,
) -> Result<Option<ContinuityInput>> {
    let Some(capture_cursor) = meta(tx, "capture_cursor")? else {
        ensure!(
            meta(tx, "stream_continuity")?.is_none(),
            "saved continuity has no capture boundary"
        );
        return Ok(None);
    };
    let (capture_session, capture_sequence, capture_digest) =
        blockzilla_bank_client::parse_cursor(&capture_cursor)?;
    ensure!(
        capture_session == source.session_id && capture_sequence > 0,
        "saved capture cursor source changed"
    );
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM stream_banks", [], |row| row.get(0))?;
    ensure!(count <= MAX_BANKS, "stream bank metadata limit exceeded");
    let mut chain = Chain::default();
    let mut eligible = BTreeSet::new();
    let mut payload_events = BTreeMap::new();
    let mut statement = tx.prepare("SELECT bank_key,identity_json,payload_frame_sha256,payload_sha256,block_height,payload_sequence,payload_event_json,application_eligible,status FROM stream_banks")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, bool>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    for row in rows {
        let (
            key,
            identity,
            frame_digest,
            content_digest,
            height,
            sequence,
            encoded_event,
            application_eligible,
            status,
        ) = row?;
        let bank: BankIdentity =
            serde_json::from_str(&identity).context("invalid saved bank identity")?;
        let event: BankEvent =
            serde_json::from_str(&encoded_event).context("invalid saved payload event")?;
        event.validate()?;
        ensure!(
            event.kind == "block"
                && bank == event.bank
                && key == bank_key(&bank)
                && height >= 0
                && event.block_height == Some(height as u64)
                && sequence > 0
                && event.sequence == sequence as u64
                && blockzilla_bank_stream_v2::is_digest(&frame_digest)
                && event.payload_sha256 == content_digest
                && application_eligible == (event.sequence >= capture_sequence)
                && matches!(
                    status.as_str(),
                    "processed" | "confirmed" | "finalized" | "invalidated"
                ),
            "saved bank metadata does not match admitted payload evidence"
        );
        if application_eligible {
            eligible.insert(frame_digest.clone());
        }
        payload_events.insert(key.clone(), event);
        chain.banks.insert(
            key,
            BankState {
                bank,
                payload_sha256: content_digest,
                payload_frame_sha256: frame_digest,
                block_height: height as u64,
                invalidated: status == "invalidated",
                rooted: status == "finalized",
                confirmed: matches!(status.as_str(), "confirmed" | "finalized"),
            },
        );
    }
    let capture_frame: (String, i64, String, String, String) = tx
        .query_row(
            "SELECT kind,sequence,sha256,bank_key,event_json FROM stream_frames WHERE cursor=?1",
            [&capture_cursor],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?
        .context("original capture frame is missing")?;
    let capture_event: BankEvent = serde_json::from_str(&capture_frame.4)?;
    let capture = chain
        .banks
        .get(&capture_frame.3)
        .context("original capture bank is missing")?
        .clone();
    ensure!(
        capture_frame.0 == "block"
            && capture_frame.1 == capture_sequence as i64
            && capture_frame.2 == capture_digest
            && capture.payload_frame_sha256 == capture_digest
            && payload_events.get(&capture_frame.3) == Some(&capture_event)
            && capture_event.sequence == capture_sequence
            && meta(tx, "capture_slot")?.as_deref() == Some(capture.bank.slot.to_string().as_str()),
        "original capture frame or boundary changed"
    );
    let mut root_cursors = BTreeMap::new();
    let mut statement = tx.prepare("SELECT cursor,sequence,sha256,bank_key,event_json FROM stream_frames WHERE kind='root' ORDER BY sequence ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (cursor, sequence, digest, key, encoded) = row?;
        let (session, cursor_sequence, cursor_digest) =
            blockzilla_bank_client::parse_cursor(&cursor)?;
        let event: BankEvent =
            serde_json::from_str(&encoded).context("invalid saved root event")?;
        event.validate()?;
        let bank = chain
            .banks
            .get(&key)
            .context("saved root payload is missing")?;
        ensure!(
            session == source.session_id
                && sequence > 0
                && cursor_sequence == sequence as u64
                && cursor_digest == digest
                && event.sequence == cursor_sequence
                && event.kind == "root"
                && event.bank == bank.bank
                && bank.rooted
                && bank.confirmed
                && !bank.invalidated
                && event.payload_frame_sha256.as_deref()
                    == Some(bank.payload_frame_sha256.as_str())
                && event.payload_sha256 == bank.payload_sha256
                && event.block_height == Some(bank.block_height),
            "saved native root control changed"
        );
        root_cursors
            .entry(bank.payload_frame_sha256.clone())
            .or_insert(cursor);
    }
    ensure!(
        chain
            .banks
            .values()
            .filter(|bank| bank.rooted)
            .all(|bank| root_cursors.contains_key(&bank.payload_frame_sha256)),
        "saved finalized bank has no exact native root control"
    );
    Ok(Some(ContinuityInput {
        chain,
        capture,
        root_cursors,
        eligible,
    }))
}

fn refresh_continuity(tx: &Transaction<'_>, source: &SourceIdentity) -> Result<()> {
    let Some(input) = continuity_input(tx, source)? else {
        return Ok(());
    };
    let previous = saved_continuity(tx)?;
    if let Some(previous) = &previous {
        ensure_same_capture(&previous.capture, &input.capture)?;
    }
    let report = continuity::evaluate(
        &input.chain,
        &input.capture,
        previous
            .as_ref()
            .and_then(|report| report.verified_root.as_ref()),
        &input.root_cursors,
        &input.eligible,
    )?;
    for digest in &report.newly_verified_payloads {
        ensure!(
            input.eligible.contains(digest),
            "continuity selected an ineligible application payload"
        );
        let changed = tx.execute("UPDATE stream_banks SET continuity_contiguous=1 WHERE payload_frame_sha256=?1 AND application_eligible=1 AND continuity_contiguous=0", [digest])?;
        ensure!(changed <= 1, "continuity payload is not unique");
    }
    let encoded = serde_json::to_string(&report)?;
    ensure!(
        encoded.len() <= MAX_PARSED_BYTES,
        "continuity report exceeds size limit"
    );
    put_meta(tx, "stream_continuity", &encoded)?;
    Ok(())
}

fn insert_frame(tx: &Transaction<'_>, frame: &Frame) -> Result<()> {
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM stream_frames", [], |row| row.get(0))?;
    ensure!(
        count < MAX_FRAMES,
        "stream frame metadata limit reached; checkpoint not advanced"
    );
    tx.execute(
        "INSERT INTO stream_frames(cursor,sequence,sha256,bank_key,kind,event_json) VALUES(?1,?2,?3,?4,?5,?6)",
        params![frame.cursor, frame.envelope.event.sequence as i64, frame.sha256,
            bank_key(&frame.envelope.event.bank), frame.envelope.event.kind,
            serde_json::to_string(&frame.envelope.event)?],
    )?;
    Ok(())
}

fn insert_payload(
    tx: &Transaction<'_>,
    frame: &Frame,
    parsed: &ParsedBlock,
    capture_sequence: u64,
) -> Result<()> {
    let event = &frame.envelope.event;
    ensure!(
        event.kind == "block",
        "bank evidence must be a processed payload"
    );
    let key = bank_key(&event.bank);
    let identity = serde_json::to_string(&event.bank)?;
    let value = parsed_value(parsed)?;
    let encoded = serde_json::to_string(&value)?;
    let previous: Option<(String, String, String, i64, i64, String, String)> = tx.query_row(
        "SELECT identity_json,payload_frame_sha256,payload_sha256,block_height,payload_sequence,parsed_json,payload_event_json FROM stream_banks WHERE bank_key=?1",
        [&key], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
    ).optional()?;
    let height = event
        .block_height
        .context("processed bank height missing")?;
    if let Some(previous) = previous {
        let (
            previous_identity,
            previous_frame_digest,
            previous_content_digest,
            previous_height,
            previous_sequence,
            previous_parsed,
            previous_event,
        ) = previous;
        let previous_value: Value = serde_json::from_str(&previous_parsed)?;
        let mut current_value = value;
        if previous_value.get("candidates").is_none()
            && previous_value.get("instructions").is_none()
        {
            // Schema 1 payloads can resume after this extension. Preserve their
            // saved facts; an ordinary control replay does not backfill new
            // parser facts or silently replace the saved parser version.
            let current = current_value
                .as_object_mut()
                .context("invalid parsed block JSON")?;
            current.remove("candidates");
            current.remove("instructions");
        }
        ensure!(
            previous_identity == identity
                && previous_frame_digest == frame.sha256
                && previous_content_digest == event.payload_sha256
                && previous_height == height as i64
                && previous_sequence == event.sequence as i64
                && previous_event == serde_json::to_string(event)?
                && parsed_core(previous_value)? == parsed_core(current_value)?,
            "saved bank or parsed payload evidence changed"
        );
        return Ok(());
    }
    let banks: i64 = tx.query_row("SELECT COUNT(*) FROM stream_banks", [], |row| row.get(0))?;
    ensure!(
        banks < MAX_BANKS,
        "stream bank metadata limit reached; checkpoint not advanced"
    );
    let eligible = event.sequence >= capture_sequence;
    let existing_events: i64 =
        tx.query_row("SELECT COUNT(*) FROM stream_parser_events", [], |row| {
            row.get(0)
        })?;
    ensure!(
        !eligible || existing_events + parsed.events.len() as i64 <= MAX_EVENTS,
        "stream parser event limit reached; checkpoint not advanced"
    );
    for (table, additional, limit) in [
        ("stream_candidates", parsed.candidates.len(), MAX_CANDIDATES),
        (
            "stream_instructions",
            parsed.instructions.len(),
            MAX_INSTRUCTIONS,
        ),
    ] {
        let count: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        ensure!(
            !eligible || count + additional as i64 <= limit,
            "stream candidate or instruction limit reached; checkpoint not advanced"
        );
    }
    let status = if has_invalidated_parent(tx, &event.bank)? {
        "invalidated"
    } else {
        "processed"
    };
    tx.execute(
        "INSERT INTO stream_banks(bank_key,identity_json,payload_frame_sha256,payload_sha256,block_height,payload_sequence,application_eligible,status,parsed_json,payload_event_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![key, identity, frame.sha256,event.payload_sha256,height as i64,event.sequence as i64,eligible,status,encoded,serde_json::to_string(event)?],
    )?;
    if eligible {
        for candidate in &parsed.candidates {
            tx.execute(
                "INSERT INTO stream_candidates(payload_frame_sha256,transaction_index,bank_key,status,signature,execution_status,log_status,log_reason,parser_version,transaction_json)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![frame.sha256,candidate.transaction_index as i64,key,status,candidate.signature,candidate.execution_status,candidate.log_status,candidate.log_reason,candidate.parser_version,serde_json::to_string(&candidate.transaction)?],
            )?;
        }
        for instruction in &parsed.instructions {
            // SQLite UNIQUE constraints treat NULL values as distinct. Use -1
            // for a top-level instruction so its exact position is unique.
            let inner_index = instruction
                .inner_instruction_index
                .map_or(-1, |index| index as i64);
            tx.execute(
                "INSERT INTO stream_instructions(payload_frame_sha256,transaction_index,outer_instruction_index,inner_instruction_index,bank_key,status,signature,stack_height,program,name,execution_status,data_base58,accounts_json,arguments_json,decode_status,decode_reason)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![frame.sha256,instruction.transaction_index as i64,instruction.outer_instruction_index as i64,inner_index,key,status,instruction.signature,instruction.stack_height,instruction.program,instruction.name,instruction.execution_status,instruction.data_base58,serde_json::to_string(&instruction.accounts)?,serde_json::to_string(&instruction.arguments)?,instruction.decode_status,instruction.decode_reason],
            )?;
        }
        for (index, event) in parsed.events.iter().enumerate() {
            tx.execute(
                "INSERT INTO stream_parser_events(payload_frame_sha256,event_index,bank_key,status,signature,transaction_index,event_json)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![frame.sha256,index as i64,key,status,event.signature,event.transaction_index as i64,serde_json::to_string(&event.event)?],
            )?;
        }
    }
    if status == "invalidated" {
        // A descendant can arrive before this payload. Once its missing parent
        // is known to be invalidated, update the retained subtree as one write.
        // Any rooted descendant makes this entire insertion fail before ACK.
        invalidate_descendants(tx, &key)?;
    }
    Ok(())
}

fn has_invalidated_parent(tx: &Transaction<'_>, bank: &BankIdentity) -> Result<bool> {
    let mut statement =
        tx.prepare("SELECT identity_json FROM stream_banks WHERE status='invalidated'")?;
    for row in statement.query_map([], |row| row.get::<_, String>(0))? {
        let parent: BankIdentity = serde_json::from_str(&row?)?;
        if bank.parent_matches(&parent) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn apply_control(tx: &Transaction<'_>, event: &BankEvent) -> Result<()> {
    let key = bank_key(&event.bank);
    let stored: (String, String, String, i64, String) = tx.query_row(
        "SELECT identity_json,payload_frame_sha256,payload_sha256,block_height,status FROM stream_banks WHERE bank_key=?1",
        [&key], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).optional()?.context("control has no saved exact bank payload evidence")?;
    ensure!(
        serde_json::from_str::<BankIdentity>(&stored.0)? == event.bank
            && event.payload_frame_sha256.as_deref() == Some(stored.1.as_str())
            && event.payload_sha256 == stored.2
            && event
                .block_height
                .is_none_or(|height| height == stored.3 as u64),
        "control does not match saved exact bank, payload or height"
    );
    match event.kind.as_str() {
        "confirmed" | "root" => {
            ensure!(
                stored.4 != "invalidated",
                "invalidated bank cannot be confirmed or rooted"
            );
            let status = if event.kind == "root" || stored.4 == "finalized" {
                "finalized"
            } else {
                "confirmed"
            };
            set_status(tx, &key, status)?;
        }
        "invalidate" => invalidate_descendants(tx, &key)?,
        _ => anyhow::bail!("unsupported application control"),
    }
    Ok(())
}

fn set_status(tx: &Transaction<'_>, key: &str, status: &str) -> Result<()> {
    for table in [
        "stream_banks",
        "stream_parser_events",
        "stream_candidates",
        "stream_instructions",
    ] {
        tx.execute(
            &format!("UPDATE {table} SET status=?1 WHERE bank_key=?2"),
            params![status, key],
        )?;
    }
    Ok(())
}

fn invalidate_descendants(tx: &Transaction<'_>, key: &str) -> Result<()> {
    let mut statement = tx.prepare("SELECT bank_key,identity_json,status FROM stream_banks")?;
    let mut banks = BTreeMap::new();
    let mut children: BTreeMap<(u64, [u8; 32]), Vec<String>> = BTreeMap::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (key, identity, status) = row?;
        let bank = serde_json::from_str::<BankIdentity>(&identity)?;
        children
            .entry((bank.parent_slot, bank.parent_blockhash))
            .or_default()
            .push(key.clone());
        banks.insert(key, (bank, status));
    }
    let mut invalidated = BTreeSet::from([key.to_string()]);
    let mut pending = vec![key.to_string()];
    while let Some(parent_key) = pending.pop() {
        let (parent, _) = banks
            .get(&parent_key)
            .context("invalidation bank missing")?;
        if let Some(descendants) = children.get(&(parent.slot, parent.blockhash)) {
            for child_key in descendants {
                if invalidated.insert(child_key.clone()) {
                    pending.push(child_key.clone());
                }
            }
        }
    }
    ensure!(
        !banks
            .iter()
            .any(|(key, (_, status))| invalidated.contains(key) && status == "finalized"),
        "invalidation conflicts with a saved rooted bank"
    );
    for key in invalidated {
        set_status(tx, &key, "invalidated")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockzilla_bank_stream_v2::fixtures;
    use std::sync::atomic::{AtomicU64, Ordering};

    const ORIGIN: &str = "https://stream.blockzilla.dev";
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TestPath(std::path::PathBuf);
    impl TestPath {
        fn new() -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "yield-stream-store-{}-{id}/capture.sqlite",
                std::process::id()
            )))
        }
    }
    impl Drop for TestPath {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }

    fn parsed() -> ParsedBlock {
        ParsedBlock {
            block_time: Some(1_700_000_000),
            transactions_scanned: 10,
            transactions_selected: 3,
            transactions_succeeded: 1,
            transactions_failed: 1,
            transactions_without_metadata: 1,
            parser_rejections: 0,
            parser_rejection_details: Vec::new(),
            candidates: Vec::new(),
            instructions: Vec::new(),
            events: vec![ParsedEvent {
                signature: "fixture-signature".into(),
                transaction_index: 7,
                event: JupiterEvent::UpdateExchangePrices {
                    log_index: 4,
                    token: "fixture-token".into(),
                    supply_exchange_price_raw: "1000000000000".into(),
                    borrow_exchange_price_raw: "1000000000000".into(),
                    borrow_rate_bps: 500,
                    borrow_apr: 0.05,
                    utilization_bps: 5_000,
                },
            }],
        }
    }

    fn store(path: &TestPath) -> StreamStore {
        StreamStore::open(&path.0, &fixtures::source(), ORIGIN).unwrap()
    }

    #[test]
    fn capture_controls_restart_and_duplicates_preserve_exact_status() {
        let path = TestPath::new();
        let mut store = store(&path);
        let payload = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&payload.cursor, &payload, &parsed())
            .unwrap();
        let confirmed = fixtures::control(&payload, 2, "confirmed", 1_100);
        store.apply(&confirmed, None, None).unwrap();
        let root = fixtures::control(&payload, 3, "root", 1_200);
        store.apply(&root, None, None).unwrap();
        store.apply(&confirmed, None, None).unwrap();
        store.apply(&root, None, None).unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["frames"], 3);
        assert_eq!(summary["events"], 1);
        assert_eq!(summary["transactions_scanned"], 10);
        assert_eq!(summary["transactions_selected"], 3);
        assert_eq!(summary["event_statuses"]["finalized"], 1);
        assert_eq!(summary["raw_payload_bytes_retained"], 0);
        drop(store);
        let mut reopened = self::store(&path);
        reopened
            .initialize(&root.cursor, &payload, &parsed())
            .unwrap();
        assert_eq!(
            reopened.cursor().unwrap().as_deref(),
            Some(root.cursor.as_str())
        );
        assert_eq!(reopened.summary().unwrap(), summary);
    }

    #[test]
    fn reconnect_keeps_parent_gap_until_a_late_payload_closes_the_rooted_path() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&capture.cursor, &capture, &parsed_with_candidate())
            .unwrap();
        assert_eq!(
            store.summary().unwrap()["continuity"]["status"],
            "awaiting_root"
        );
        let later = fixtures::block(2, 102, 3, 101, 1_002);
        store
            .apply(&later, None, Some(&parsed_with_candidate()))
            .unwrap();
        let root = fixtures::control(&later, 3, "root", 1_200);
        store.apply(&root, None, None).unwrap();
        let gap = store.summary().unwrap();
        assert_eq!(gap["continuity"]["status"], "gap");
        assert_eq!(gap["continuity"]["missing_parent"]["parent_slot"], 101);
        assert!(gap["continuity"]["verified_root"].is_null());
        assert_eq!(gap["continuity_covered_banks"], 0);
        assert_eq!(gap["candidates"], 2);
        drop(store);
        let mut store = self::store(&path);
        assert_eq!(store.summary().unwrap(), gap);
        let missing = fixtures::block(4, 101, 2, 100, 1_001);
        store
            .apply(&missing, None, Some(&parsed_with_candidate()))
            .unwrap();
        let closed = store.summary().unwrap();
        assert_eq!(closed["continuity"]["status"], "contiguous");
        assert_eq!(closed["continuity"]["verified_root"]["bank"]["slot"], 102);
        assert_eq!(
            closed["continuity"]["verified_root"]["root_cursor"],
            root.cursor
        );
        assert_eq!(closed["continuity_covered_banks"], 3);
        assert_eq!(closed["candidate_statuses"]["finalized"], 1);
        assert_eq!(closed["candidate_statuses"]["processed"], 2);
        // Slot numbers can jump when slots produce no block. Native block
        // height must still increase by exactly one for the produced parent.
        let next = fixtures::block(5, 105, 5, 102, 1_003);
        store
            .apply(&next, None, Some(&parsed_with_candidate()))
            .unwrap();
        let next_root = fixtures::control(&next, 6, "root", 1_300);
        store.apply(&next_root, None, None).unwrap();
        let contiguous = store.summary().unwrap();
        assert_eq!(contiguous["continuity"]["status"], "contiguous");
        assert_eq!(
            contiguous["continuity"]["verified_root"]["bank"]["slot"],
            105
        );
        assert_eq!(contiguous["continuity_covered_banks"], 4);
        drop(store);
        assert_eq!(self::store(&path).summary().unwrap(), contiguous);
    }

    fn with_parent_hash(mut frame: Frame, hash: [u8; 32]) -> Frame {
        let metadata_length = u32::from_le_bytes(frame.wire[8..12].try_into().unwrap()) as usize;
        let payload = frame.wire[16 + metadata_length..].to_vec();
        frame.envelope.event.bank.parent_blockhash = hash;
        frame.envelope.marker["produced_blocks"] = json!([frame.envelope.event.bank]);
        blockzilla_bank_stream_v2::encode(frame.envelope, &payload).unwrap()
    }

    #[test]
    fn wrong_parent_hash_keeps_the_verified_root_across_restart() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&capture.cursor, &capture, &parsed_with_candidate())
            .unwrap();
        let first_root = fixtures::control(&capture, 2, "root", 1_100);
        store.apply(&first_root, None, None).unwrap();
        let wrong = with_parent_hash(fixtures::block(3, 101, 2, 100, 1_001), [42; 32]);
        store
            .apply(&wrong, None, Some(&parsed_with_candidate()))
            .unwrap();
        let root = fixtures::control(&wrong, 4, "root", 1_200);
        store.apply(&root, None, None).unwrap();
        let gap = store.summary().unwrap();
        assert_eq!(gap["continuity"]["status"], "gap");
        assert_eq!(
            gap["continuity"]["verified_root"]["root_cursor"],
            first_root.cursor
        );
        assert_eq!(
            gap["continuity"]["missing_parent"]["parent_blockhash"],
            bs58::encode([42; 32]).into_string()
        );
        assert_eq!(gap["continuity_covered_banks"], 1);
        assert_eq!(gap["candidates"], 2);
        assert_eq!(gap["candidate_statuses"]["finalized"], 2);
        drop(store);
        assert_eq!(self::store(&path).summary().unwrap(), gap);
    }

    #[test]
    fn missing_continuity_migrates_from_the_original_capture_and_native_roots() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&capture.cursor, &capture, &parsed())
            .unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        store.apply(&child, None, Some(&parsed())).unwrap();
        let root = fixtures::control(&child, 3, "root", 1_200);
        store.apply(&root, None, None).unwrap();
        store
            .db
            .execute_batch(
                "DELETE FROM stream_meta WHERE key='stream_continuity';
            ALTER TABLE stream_banks DROP COLUMN continuity_contiguous;",
            )
            .unwrap();
        drop(store);
        let mut store = self::store(&path);
        let migrated = store.summary().unwrap();
        assert_eq!(migrated["continuity"]["capture"]["bank"]["slot"], 100);
        assert_eq!(migrated["continuity"]["verified_root"]["bank"]["slot"], 101);
        assert_eq!(migrated["continuity_covered_banks"], 2);
        let duplicate_root = fixtures::control(&child, 4, "root", 1_300);
        store.apply(&duplicate_root, None, None).unwrap();
        assert_eq!(
            store.summary().unwrap()["continuity"]["verified_root"]["root_cursor"],
            root.cursor
        );
        assert_eq!(store.summary().unwrap()["continuity_covered_banks"], 2);
        drop(store);
        let store = self::store(&path);
        store
            .db
            .execute("DELETE FROM stream_frames WHERE cursor=?1", [&root.cursor])
            .unwrap();
        drop(store);
        assert!(StreamStore::open(&path.0, &fixtures::source(), ORIGIN).is_err());
    }

    #[test]
    fn continuity_write_failure_rolls_back_root_status_and_intake_cursor() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&capture.cursor, &capture, &parsed_with_candidate())
            .unwrap();
        let before = store.summary().unwrap();
        store.db.execute_batch("CREATE TRIGGER reject_continuity BEFORE UPDATE ON stream_meta WHEN NEW.key='stream_continuity' BEGIN SELECT RAISE(ABORT,'fixture continuity write failure'); END;").unwrap();
        let root = fixtures::control(&capture, 2, "root", 1_100);
        assert!(store.apply(&root, None, None).is_err());
        assert_eq!(store.summary().unwrap(), before);
        assert_eq!(
            store.cursor().unwrap().as_deref(),
            Some(capture.cursor.as_str())
        );
    }

    #[test]
    fn restart_rejects_changed_continuity_status_and_highest_root() {
        for mutation in [
            "UPDATE stream_meta SET value=json_set(value,'$.status','contiguous') WHERE key='stream_continuity'",
            "UPDATE stream_meta SET value=json_set(value,'$.highest_root',NULL) WHERE key='stream_continuity'",
            "UPDATE stream_meta SET value=json_set(value,'$.highest_root.bank.slot',999) WHERE key='stream_continuity'",
        ] {
            let path = TestPath::new();
            let mut store = store(&path);
            let capture = fixtures::block(1, 100, 1, 99, 1_000);
            store.initialize(&capture.cursor, &capture, &parsed()).unwrap();
            let first_root = fixtures::control(&capture, 2, "root", 1_100);
            store.apply(&first_root, None, None).unwrap();
            let later = fixtures::block(3, 102, 3, 101, 1_002);
            store.apply(&later, None, Some(&parsed())).unwrap();
            let root = fixtures::control(&later, 4, "root", 1_200);
            store.apply(&root, None, None).unwrap();
            assert_eq!(store.summary().unwrap()["continuity"]["status"], "gap");
            store.db.execute_batch(mutation).unwrap();
            drop(store);
            assert!(StreamStore::open(&path.0, &fixtures::source(), ORIGIN).is_err(), "accepted mutation: {mutation}");
        }
    }

    #[test]
    fn restart_rejects_changed_coverage_flags() {
        for mutation in [
            "UPDATE stream_banks SET continuity_contiguous=0 WHERE bank_key='100:1'",
            "UPDATE stream_banks SET continuity_contiguous=1 WHERE bank_key='102:3'",
        ] {
            let path = TestPath::new();
            let mut store = store(&path);
            let capture = fixtures::block(1, 100, 1, 99, 1_000);
            store
                .initialize(&capture.cursor, &capture, &parsed())
                .unwrap();
            let first_root = fixtures::control(&capture, 2, "root", 1_100);
            store.apply(&first_root, None, None).unwrap();
            let later = fixtures::block(3, 102, 3, 101, 1_002);
            store.apply(&later, None, Some(&parsed())).unwrap();
            let root = fixtures::control(&later, 4, "root", 1_200);
            store.apply(&root, None, None).unwrap();
            assert_eq!(store.summary().unwrap()["continuity_covered_banks"], 1);
            store.db.execute_batch(mutation).unwrap();
            drop(store);
            assert!(
                StreamStore::open(&path.0, &fixtures::source(), ORIGIN).is_err(),
                "accepted mutation: {mutation}"
            );
        }
    }

    #[test]
    fn restart_rejects_a_coverage_flag_on_an_excluded_dependency() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(3, 300, 3, 299, 3_000);
        store
            .initialize(&capture.cursor, &capture, &parsed())
            .unwrap();
        let old = fixtures::block(1, 100, 1, 99, 1_000);
        let root = fixtures::control(&old, 4, "root", 1_200);
        store.apply(&root, Some((&old, &parsed())), None).unwrap();
        assert_eq!(store.summary().unwrap()["continuity_covered_banks"], 0);
        store
            .db
            .execute(
                "UPDATE stream_banks SET continuity_contiguous=1 WHERE bank_key='100:1'",
                [],
            )
            .unwrap();
        drop(store);
        let error = StreamStore::open(&path.0, &fixtures::source(), ORIGIN)
            .err()
            .unwrap();
        assert!(error.to_string().contains("ineligible bank"));
    }

    #[test]
    fn late_duplicate_parent_cannot_damage_the_verified_prefix() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&capture.cursor, &capture, &parsed_with_candidate())
            .unwrap();
        let middle = fixtures::block(2, 101, 2, 100, 1_001);
        store
            .apply(&middle, None, Some(&parsed_with_candidate()))
            .unwrap();
        let tip = fixtures::block(3, 102, 3, 101, 1_002);
        store
            .apply(&tip, None, Some(&parsed_with_candidate()))
            .unwrap();
        let root = fixtures::control(&tip, 4, "root", 1_200);
        store.apply(&root, None, None).unwrap();
        let before = store.summary().unwrap();
        assert_eq!(before["continuity"]["status"], "contiguous");
        assert_eq!(before["continuity_covered_banks"], 3);
        // Same canonical parent slot/hash, but a second exact native bank
        // identity. Do not choose one parent silently or keep old coverage.
        let duplicate = fixtures::block(5, 101, 99, 100, 1_001);
        let error = store
            .apply(&duplicate, None, Some(&parsed_with_candidate()))
            .unwrap_err();
        assert!(error.to_string().contains("ambiguous_parent_bank"));
        assert_eq!(store.summary().unwrap(), before);
        assert_eq!(
            store.cursor().unwrap().as_deref(),
            Some(root.cursor.as_str())
        );
        drop(store);
        assert_eq!(self::store(&path).summary().unwrap(), before);
    }

    fn parsed_with_rejection() -> ParsedBlock {
        let mut parsed = parsed();
        parsed.transactions_selected += 1;
        parsed.transactions_succeeded += 1;
        parsed.parser_rejections = 1;
        parsed.parser_rejection_details.push(ParserRejection {
            signature: "rejected-fixture-signature".into(),
            transaction_index: 8,
            reason: "transaction program invocation logs are incomplete".into(),
        });
        parsed
    }

    fn parsed_with_candidate() -> ParsedBlock {
        let mut parsed = parsed_with_rejection();
        parsed.candidates.push(ParsedCandidate {
            signature: "rejected-fixture-signature".into(),
            transaction_index: 8,
            execution_status: "succeeded".into(),
            log_status: "truncated".into(),
            log_reason: Some("transaction program invocation logs are incomplete".into()),
            parser_version: "jupiter-instructions-v1".into(),
            transaction: json!({"transaction": {"signatures": ["rejected-fixture-signature"], "message": {"instructions": []}}, "meta": {"err": null, "logMessages": ["Log truncated"]}}),
        });
        parsed.instructions.push(ParsedInstruction {
            signature: "rejected-fixture-signature".into(),
            transaction_index: 8,
            outer_instruction_index: 1,
            inner_instruction_index: None,
            stack_height: None,
            program: "fixture-program".into(),
            name: "operate".into(),
            execution_status: "committed_top_level".into(),
            data_base58: "1".into(),
            accounts: vec!["fixture-account".into()],
            arguments: json!({"supply_amount": "100"}),
            decode_status: "decoded".into(),
            decode_reason: None,
        });
        parsed
    }

    #[test]
    fn truncated_logs_keep_candidates_and_instructions_with_native_bank_status() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed_with_candidate())
            .unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["candidates"], 1);
        assert_eq!(summary["instructions"], 1);
        assert_eq!(summary["candidate_log_statuses"]["truncated"], 1);
        assert_eq!(summary["instruction_decode_statuses"]["decoded"], 1);
        let confirmed = fixtures::control(&parent, 2, "confirmed", 1_100);
        store.apply(&confirmed, None, None).unwrap();
        let child = fixtures::block(3, 101, 2, 100, 1_001);
        store
            .apply(&child, None, Some(&parsed_with_candidate()))
            .unwrap();
        let invalid = fixtures::control(&child, 4, "invalidate", 1_200);
        store.apply(&invalid, None, None).unwrap();
        let summary = store.summary().unwrap();
        for field in ["candidate_statuses", "instruction_statuses"] {
            assert_eq!(summary[field]["confirmed"], 1);
            assert_eq!(summary[field]["invalidated"], 1);
        }
        let root = fixtures::control(&parent, 5, "root", 1_300);
        store
            .apply(&root, Some((&parent, &parsed_with_candidate())), None)
            .unwrap();
        store.apply(&root, None, None).unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["candidates"], 2);
        assert_eq!(summary["instructions"], 2);
        assert_eq!(summary["candidate_statuses"]["finalized"], 1);
        assert_eq!(summary["instruction_statuses"]["finalized"], 1);
        let instruction_position: i64 = store.db.query_row(
            "SELECT inner_instruction_index FROM stream_instructions WHERE payload_frame_sha256=?1",
            [&parent.sha256], |row| row.get(0),
        ).unwrap();
        assert_eq!(instruction_position, -1);
    }

    #[test]
    fn candidate_and_instruction_bounds_fail_without_advancing_checkpoint() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed_with_candidate())
            .unwrap();
        let before = store.summary().unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        let mut oversized = parsed_with_candidate();
        oversized.candidates[0].transaction["oversized"] = json!("x".repeat(MAX_CANDIDATE_BYTES));
        assert!(store
            .apply(&child, None, Some(&oversized))
            .unwrap_err()
            .to_string()
            .contains("candidate exceeds size"));
        let mut too_many_accounts = parsed_with_candidate();
        too_many_accounts.instructions[0].accounts = vec!["account".into(); 257];
        assert!(store.apply(&child, None, Some(&too_many_accounts)).is_err());
        let mut duplicate = parsed_with_candidate();
        duplicate
            .instructions
            .push(parsed_with_candidate().instructions.remove(0));
        assert!(store.apply(&child, None, Some(&duplicate)).is_err());
        let mut orphan = parsed_with_candidate();
        orphan.instructions[0].signature = "other-signature".into();
        assert!(store.apply(&child, None, Some(&orphan)).is_err());
        assert_eq!(store.summary().unwrap(), before);
        let confirmed = fixtures::control(&parent, 2, "confirmed", 1_100);
        let mut changed = parsed_with_candidate();
        changed.instructions[0].arguments = json!({"supply_amount": "999"});
        assert!(store
            .apply(&confirmed, Some((&parent, &changed)), None)
            .is_err());
        assert_eq!(store.summary().unwrap(), before);
    }

    #[test]
    fn parsed_block_budget_includes_retained_normalized_transactions() {
        let mut parsed = parsed();
        parsed.transactions_scanned = 17;
        parsed.transactions_selected = 17;
        parsed.transactions_succeeded = 17;
        parsed.transactions_failed = 0;
        parsed.transactions_without_metadata = 0;
        parsed.events.clear();
        for index in 0..17 {
            let mut candidate = parsed_with_candidate().candidates.remove(0);
            candidate.transaction_index = index;
            candidate.transaction = json!({"fixture": "x".repeat(1_000_000)});
            parsed.candidates.push(candidate);
        }
        assert!(parsed_value(&parsed)
            .unwrap_err()
            .to_string()
            .contains("block exceeds size"));
    }

    #[test]
    fn instruction_write_failure_rolls_back_candidate_bank_frame_and_cursor() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed_with_candidate())
            .unwrap();
        store.db.execute_batch("CREATE TRIGGER reject_instruction BEFORE INSERT ON stream_instructions BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
        let before = store.summary().unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        assert!(store
            .apply(&child, None, Some(&parsed_with_candidate()))
            .is_err());
        assert_eq!(store.summary().unwrap(), before);
    }

    #[test]
    fn failed_unknown_and_unsupported_instruction_evidence_is_retained() {
        let path = TestPath::new();
        let mut store = store(&path);
        let payload = fixtures::block(1, 100, 1, 99, 1_000);
        let mut parsed = parsed_with_candidate();
        for (index, status, instruction_status) in [
            (7, "failed", "failed_transaction"),
            (9, "unknown", "unknown_transaction"),
        ] {
            let mut candidate = parsed_with_candidate().candidates.remove(0);
            candidate.transaction_index = index;
            candidate.signature = format!("fixture-{status}");
            candidate.execution_status = status.into();
            candidate.log_status = "unavailable".into();
            candidate.log_reason = None;
            let mut instruction = parsed_with_candidate().instructions.remove(0);
            instruction.transaction_index = index;
            instruction.signature = candidate.signature.clone();
            instruction.execution_status = instruction_status.into();
            instruction.decode_status = "unsupported".into();
            instruction.decode_reason = Some("unknown instruction discriminator".into());
            parsed.candidates.push(candidate);
            parsed.instructions.push(instruction);
        }
        store
            .initialize(&payload.cursor, &payload, &parsed)
            .unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["candidates"], 3);
        assert_eq!(summary["instructions"], 3);
        assert_eq!(summary["candidate_execution_statuses"]["failed"], 1);
        assert_eq!(summary["candidate_execution_statuses"]["unknown"], 1);
        assert_eq!(
            summary["instruction_execution_statuses"]["failed_transaction"],
            1
        );
        assert_eq!(
            summary["instruction_execution_statuses"]["unknown_transaction"],
            1
        );
        assert_eq!(summary["instruction_decode_statuses"]["unsupported"], 2);
    }

    #[test]
    fn candidate_and_instruction_capacity_stops_before_checkpoint_advance() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed_with_candidate())
            .unwrap();
        // Small direct fixture records fill the row budget. They are not used
        // as native controls, account state, or yield evidence.
        store.db.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<99999)
            INSERT INTO stream_candidates(payload_frame_sha256,transaction_index,bank_key,status,signature,execution_status,log_status,parser_version,transaction_json)
            SELECT 'filler:'||x,0,'100:1','processed','fixture','unknown','unavailable','fixture','{}' FROM n;").unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        let error = store
            .apply(&child, None, Some(&parsed_with_candidate()))
            .unwrap_err();
        assert!(error.to_string().contains("candidate or instruction limit"));
        assert_eq!(
            store.cursor().unwrap().as_deref(),
            Some(parent.cursor.as_str())
        );
        assert_eq!(store.summary().unwrap()["banks"], 1);
        store.db.execute_batch("INSERT INTO stream_instructions(payload_frame_sha256,transaction_index,outer_instruction_index,inner_instruction_index,bank_key,status,signature,program,name,execution_status,data_base58,accounts_json,arguments_json,decode_status)
            SELECT payload_frame_sha256,transaction_index,0,-1,bank_key,status,signature,'fixture','fixture','unknown_transaction','','[]','{}','unsupported' FROM stream_candidates WHERE payload_frame_sha256 LIKE 'filler:%';").unwrap();
        // Keep all candidate foreign keys. The second source payload contains
        // the existing candidate plus one extra CPI instruction, so candidate
        // storage can fit while instruction storage cannot.
        store
            .db
            .execute_batch(
                "DELETE FROM stream_instructions WHERE payload_frame_sha256='filler:99999';
            DELETE FROM stream_candidates WHERE payload_frame_sha256='filler:99999';",
            )
            .unwrap();
        let mut parsed = parsed_with_candidate();
        let mut inner = parsed_with_candidate().instructions.remove(0);
        inner.inner_instruction_index = Some(0);
        inner.execution_status = "attempted_cpi_unproven".into();
        parsed.instructions.push(inner);
        let error = store.apply(&child, None, Some(&parsed)).unwrap_err();
        assert!(error.to_string().contains("candidate or instruction limit"));
        assert_eq!(
            store.cursor().unwrap().as_deref(),
            Some(parent.cursor.as_str())
        );
        assert_eq!(store.summary().unwrap()["banks"], 1);
    }

    #[test]
    fn rejection_diagnostics_are_bounded_and_kept_with_the_exact_bank() {
        let path = TestPath::new();
        let mut store = store(&path);
        let payload = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&payload.cursor, &payload, &parsed_with_rejection())
            .unwrap();
        let reason: String = store
            .db
            .query_row(
                "SELECT json_extract(parsed_json,'$.parser_rejection_details[0].reason') FROM stream_banks WHERE payload_frame_sha256=?1",
                [&payload.sha256],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "transaction program invocation logs are incomplete");
        let before = store.summary().unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        let mut oversized = parsed_with_rejection();
        oversized.parser_rejection_details[0].reason = "x".repeat(513);
        assert!(store.apply(&child, None, Some(&oversized)).is_err());
        let mut missing = parsed_with_rejection();
        missing.parser_rejection_details.clear();
        assert!(store.apply(&child, None, Some(&missing)).is_err());
        let mut wrong_index = parsed_with_rejection();
        wrong_index.parser_rejection_details[0].transaction_index =
            wrong_index.transactions_scanned;
        assert!(store.apply(&child, None, Some(&wrong_index)).is_err());
        assert_eq!(store.summary().unwrap(), before);
    }

    #[test]
    fn old_capture_without_diagnostics_resumes_without_changing_saved_facts() {
        let path = TestPath::new();
        let mut store = store(&path);
        let payload = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&payload.cursor, &payload, &parsed_with_rejection())
            .unwrap();
        store
            .db
            .execute_batch(
                "UPDATE stream_banks SET parsed_json=json_remove(parsed_json,'$.parser_rejection_details','$.candidates','$.instructions');",
            )
            .unwrap();
        drop(store);
        let mut store = self::store(&path);
        let confirmed = fixtures::control(&payload, 2, "confirmed", 1_100);
        store
            .apply(&confirmed, Some((&payload, &parsed_with_candidate())), None)
            .unwrap();
        assert_eq!(store.summary().unwrap()["parser_rejections"], 1);
        assert_eq!(store.summary().unwrap()["events"], 1);
        assert_eq!(store.summary().unwrap()["candidates"], 0);
        assert_eq!(store.summary().unwrap()["instructions"], 0);
        let saved: String = store
            .db
            .query_row("SELECT parsed_json FROM stream_banks", [], |row| row.get(0))
            .unwrap();
        let saved: Value = serde_json::from_str(&saved).unwrap();
        for field in ["parser_rejection_details", "candidates", "instructions"] {
            assert!(saved.get(field).is_none());
        }
        let before = store.summary().unwrap();
        let root = fixtures::control(&payload, 3, "root", 1_200);
        let mut changed = parsed_with_rejection();
        changed.block_time = Some(1_700_000_001);
        assert!(store
            .apply(&root, Some((&payload, &changed)), None)
            .is_err());
        assert_eq!(store.summary().unwrap(), before);
    }

    #[test]
    fn capture_excludes_older_control_dependency_events_and_metrics() {
        let path = TestPath::new();
        let mut store = store(&path);
        let capture = fixtures::block(3, 300, 3, 299, 3_000);
        store
            .initialize(&capture.cursor, &capture, &parsed())
            .unwrap();
        let old = fixtures::block(1, 100, 1, 99, 1_000);
        let control = fixtures::control(&old, 4, "root", 1_200);
        store
            .apply(&control, Some((&old, &parsed())), None)
            .unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["banks"], 2);
        assert_eq!(summary["frames"], 2);
        assert_eq!(summary["events"], 1);
        assert_eq!(summary["transactions_scanned"], 10);
        assert_eq!(summary["event_statuses"]["processed"], 1);
        assert!(summary["event_statuses"].get("finalized").is_none());
        let recorded: String = store
            .db
            .query_row(
                "SELECT payload_event_json FROM stream_banks WHERE payload_frame_sha256=?1",
                [old.sha256],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<BankEvent>(&recorded).unwrap(),
            old.envelope.event
        );
    }

    #[test]
    fn invalidation_uses_parent_hashes_and_does_not_merge_same_slot_banks() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        let sibling = fixtures::block(3, 101, 3, 100, 1_001);
        let grandchild = fixtures::block(4, 102, 4, 101, 1_002);
        store
            .initialize(&parent.cursor, &parent, &parsed())
            .unwrap();
        for frame in [&child, &sibling, &grandchild] {
            store.apply(frame, None, Some(&parsed())).unwrap();
        }
        let invalid = fixtures::control(&child, 5, "invalidate", 1_100);
        store.apply(&invalid, None, None).unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["events"], 4);
        assert_eq!(summary["event_statuses"]["invalidated"], 2);
        assert_eq!(summary["event_statuses"]["processed"], 2);
        let status: String = store
            .db
            .query_row(
                "SELECT status FROM stream_banks WHERE bank_key='101:3'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");
    }

    #[test]
    fn root_finalizes_only_exact_bank_and_ancestor_invalidation_rolls_back() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        store
            .initialize(&parent.cursor, &parent, &parsed())
            .unwrap();
        store.apply(&child, None, Some(&parsed())).unwrap();
        let root = fixtures::control(&child, 3, "root", 1_100);
        store.apply(&root, None, None).unwrap();
        let before = store.summary().unwrap();
        assert_eq!(before["event_statuses"]["processed"], 1);
        assert_eq!(before["event_statuses"]["finalized"], 1);
        let invalid = fixtures::control(&parent, 4, "invalidate", 1_200);
        assert!(store.apply(&invalid, None, None).is_err());
        assert_eq!(store.summary().unwrap(), before);
    }

    #[test]
    fn new_child_of_invalidated_parent_cannot_become_confirmed() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed())
            .unwrap();
        let invalid = fixtures::control(&parent, 2, "invalidate", 1_100);
        store.apply(&invalid, None, None).unwrap();
        let child = fixtures::block(3, 101, 2, 100, 1_001);
        store.apply(&child, None, Some(&parsed())).unwrap();
        let before = store.summary().unwrap();
        assert_eq!(before["event_statuses"]["invalidated"], 2);
        let confirmed = fixtures::control(&child, 4, "confirmed", 1_200);
        assert!(store.apply(&confirmed, None, None).is_err());
        assert_eq!(store.summary().unwrap(), before);
    }

    #[test]
    fn late_invalidated_parent_updates_already_saved_descendants() {
        let path = TestPath::new();
        let mut store = store(&path);
        let ancestor = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&ancestor.cursor, &ancestor, &parsed())
            .unwrap();
        let invalid = fixtures::control(&ancestor, 2, "invalidate", 1_100);
        store.apply(&invalid, None, None).unwrap();
        let descendant = fixtures::block(3, 102, 3, 101, 1_002);
        store.apply(&descendant, None, Some(&parsed())).unwrap();
        assert_eq!(store.summary().unwrap()["event_statuses"]["processed"], 1);
        let parent = fixtures::block(4, 101, 2, 100, 1_001);
        store.apply(&parent, None, Some(&parsed())).unwrap();
        let summary = store.summary().unwrap();
        assert_eq!(summary["events"], 3);
        assert_eq!(summary["event_statuses"]["invalidated"], 3);
        assert!(summary["event_statuses"].get("processed").is_none());
    }

    #[test]
    fn late_invalidated_parent_conflicting_with_rooted_descendant_rolls_back() {
        let path = TestPath::new();
        let mut store = store(&path);
        let ancestor = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&ancestor.cursor, &ancestor, &parsed())
            .unwrap();
        let invalid = fixtures::control(&ancestor, 2, "invalidate", 1_100);
        store.apply(&invalid, None, None).unwrap();
        let descendant = fixtures::block(3, 102, 3, 101, 1_002);
        store.apply(&descendant, None, Some(&parsed())).unwrap();
        let root = fixtures::control(&descendant, 4, "root", 1_200);
        store.apply(&root, None, None).unwrap();
        let before = store.summary().unwrap();
        let parent = fixtures::block(5, 101, 2, 100, 1_001);
        assert!(store.apply(&parent, None, Some(&parsed())).is_err());
        assert_eq!(store.summary().unwrap(), before);
        assert_eq!(
            store.cursor().unwrap().as_deref(),
            Some(root.cursor.as_str())
        );
    }

    #[test]
    fn gap_mismatched_height_changed_source_and_changed_origin_are_rejected() {
        let path = TestPath::new();
        let mut store = store(&path);
        let payload = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&payload.cursor, &payload, &parsed())
            .unwrap();
        let before = store.summary().unwrap();
        let gap = fixtures::control(&payload, 3, "confirmed", 1_100);
        assert!(store.apply(&gap, None, None).is_err());
        let changed_height_payload = fixtures::block(1, 100, 1, 99, 1_001);
        let control = fixtures::control(&changed_height_payload, 2, "root", 1_100);
        assert!(store.apply(&control, None, None).is_err());
        assert_eq!(store.summary().unwrap(), before);
        drop(store);
        let mut source = fixtures::source();
        source.session_id = "different-session".into();
        assert!(StreamStore::open(&path.0, &source, ORIGIN).is_err());
        assert!(StreamStore::open(&path.0, &fixtures::source(), "https://other.invalid").is_err());
        assert_eq!(self::store(&path).summary().unwrap(), before);
    }

    #[test]
    fn event_write_failure_rolls_back_bank_frame_and_cursor_together() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed())
            .unwrap();
        store.db.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON stream_parser_events BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
        let before = store.summary().unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        assert!(store.apply(&child, None, Some(&parsed())).is_err());
        assert_eq!(store.summary().unwrap(), before);
        store.db.execute_batch("DROP TRIGGER reject_event").unwrap();
        store.apply(&child, None, Some(&parsed())).unwrap();
        assert_eq!(store.summary().unwrap()["events"], 2);
    }

    #[test]
    fn bank_capacity_failure_does_not_advance_cursor() {
        let path = TestPath::new();
        let mut store = store(&path);
        let parent = fixtures::block(1, 100, 1, 99, 1_000);
        store
            .initialize(&parent.cursor, &parent, &parsed())
            .unwrap();
        // Fill only the metadata table. These direct fixture rows have no
        // application events and are not fed to any native control.
        store.db.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<9999)
            INSERT INTO stream_banks(bank_key,identity_json,payload_frame_sha256,payload_sha256,block_height,payload_sequence,payload_event_json,application_eligible,status,parsed_json)
            SELECT 'filler:'||x,'{}','filler:'||x,'digest',1,1,'{}',0,'processed','{}' FROM n;").unwrap();
        let child = fixtures::block(2, 101, 2, 100, 1_001);
        let error = store.apply(&child, None, Some(&parsed())).unwrap_err();
        assert!(error.to_string().contains("bank metadata limit"));
        assert_eq!(
            store.cursor().unwrap().as_deref(),
            Some(parent.cursor.as_str())
        );
        assert_eq!(store.summary().unwrap()["frames"], 1);
    }
}
