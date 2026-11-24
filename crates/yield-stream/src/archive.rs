//! Bounded replay of a trusted local Compact V2 archive range manifest.

use anyhow::{ensure, Context, Result};
use blockzilla_decoder::{
    archive::{decode_bundle, ExpectedArchiveBlock, MAX_BUNDLE_BYTES},
    digest, DecodeOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_transaction_status_client_types::{
    option_serializer::OptionSerializer, EncodedTransaction, EncodedTransactionWithStatusMeta,
    UiMessage,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::Path,
};
use yield_parser::{
    jupiter_ledger::{EARN_PROGRAM, LIQUIDITY_PROGRAM},
    kamino_ledger,
    ledger::{parse_transaction, ExecutionStatus, LogStatus, ReturnStatus},
    LedgerMeasurement,
};

const MANIFEST_SCHEMA: &str = "ferno-source-bound-local-binary-range-v1";
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_BLOCKS: usize = 512;
const MAX_SLOT_SPAN: u64 = 4_096;
const MAX_RANGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RESULT_ROWS: usize = 50_000;

const PROGRAMS: [&str; 3] = [EARN_PROGRAM, LIQUIDITY_PROGRAM, kamino_ledger::PROGRAM];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema: String,
    generation: String,
    verification_level: String,
    first_slot: u64,
    last_slot: u64,
    omitted_slots: Vec<u64>,
    output_bytes_including_anchor: u64,
    anchor: Block,
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Block {
    slot: u64,
    parent_slot: u64,
    #[serde(rename = "blockhash")]
    block_hash: String,
    #[serde(rename = "previousBlockhash")]
    previous_block_hash: String,
    block_height: u64,
    block_time: Option<i64>,
    file: String,
    wire_bytes: u64,
    wire_sha256: String,
    transaction_count: usize,
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= limit as u64,
        "archive input is not a regular file within its byte limit"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "archive input grew past its byte limit"
    );
    Ok(bytes)
}

fn check_manifest(manifest: &Manifest) -> Result<()> {
    ensure!(
        manifest.schema == MANIFEST_SCHEMA,
        "unsupported archive manifest schema"
    );
    ensure!(
        !manifest.generation.is_empty()
            && manifest.generation.len() <= 256
            && !manifest.verification_level.is_empty()
            && manifest.verification_level.len() <= 64,
        "invalid archive manifest provenance"
    );
    ensure!(
        manifest.anchor.slot < manifest.first_slot
            && manifest.first_slot <= manifest.last_slot
            && manifest.last_slot - manifest.first_slot < MAX_SLOT_SPAN
            && (1..=MAX_BLOCKS).contains(&manifest.blocks.len()),
        "archive range is outside replay limits"
    );
    let mut present = BTreeSet::new();
    let mut previous = &manifest.anchor;
    let mut total_bytes = previous.wire_bytes;
    for block in &manifest.blocks {
        ensure!(
            (manifest.first_slot..=manifest.last_slot).contains(&block.slot)
                && block.slot > previous.slot
                && block.parent_slot == previous.slot
                && block.previous_block_hash == previous.block_hash
                && previous.block_height.checked_add(1) == Some(block.block_height),
            "archive parent/hash/height mismatch at slot {}",
            block.slot
        );
        total_bytes = total_bytes
            .checked_add(block.wire_bytes)
            .context("archive byte count overflow")?;
        present.insert(block.slot);
        previous = block;
    }
    let omissions = (manifest.first_slot..=manifest.last_slot)
        .filter(|slot| !present.contains(slot))
        .collect::<Vec<_>>();
    ensure!(
        omissions == manifest.omitted_slots,
        "archive omitted slots differ from manifest range"
    );
    ensure!(
        total_bytes == manifest.output_bytes_including_anchor && total_bytes <= MAX_RANGE_BYTES,
        "archive range byte count differs from manifest or exceeds its limit"
    );
    Ok(())
}

fn decode_block(directory: &Path, expected: &Block) -> Result<blockzilla_decoder::DecodedBlock> {
    // A manifest entry names only the regular sibling file for its exact slot.
    ensure!(
        expected.file == format!("{}.bin", expected.slot),
        "invalid archive block file name"
    );
    ensure!(
        (1..=MAX_BUNDLE_BYTES as u64).contains(&expected.wire_bytes),
        "archive block size is outside replay limits"
    );
    let bytes = read_bounded(
        &directory.join(&expected.file),
        expected.wire_bytes as usize,
    )?;
    ensure!(
        bytes.len() as u64 == expected.wire_bytes && digest(&bytes) == expected.wire_sha256,
        "archive block bytes differ from manifest"
    );
    let block = decode_bundle(
        &bytes,
        &ExpectedArchiveBlock {
            slot: expected.slot,
            blockhash: expected.block_hash.clone(),
            parent_slot: Some(expected.parent_slot),
            previous_blockhash: Some(expected.previous_block_hash.clone()),
            block_height: Some(expected.block_height),
        },
        &DecodeOptions::default(),
    )?;
    ensure!(
        block.total_transactions == expected.transaction_count
            && block.block.block_time == expected.block_time,
        "archive block metadata differs from manifest"
    );
    Ok(block)
}

fn touches_jupiter(program: &str) -> bool {
    matches!(program, EARN_PROGRAM | LIQUIDITY_PROGRAM)
}

fn touches(tx: &EncodedTransactionWithStatusMeta, contains: impl Fn(&str) -> bool) -> bool {
    let EncodedTransaction::Json(transaction) = &tx.transaction else {
        return false;
    };
    match &transaction.message {
        UiMessage::Parsed(message) => message.account_keys.iter().any(|key| contains(&key.pubkey)),
        UiMessage::Raw(message) => {
            if message.account_keys.iter().any(|key| contains(key)) {
                return true;
            }
            tx.meta
                .as_ref()
                .is_some_and(|meta| match &meta.loaded_addresses {
                    OptionSerializer::Some(loaded) => loaded
                        .writable
                        .iter()
                        .chain(&loaded.readonly)
                        .any(|key| contains(key)),
                    _ => false,
                })
        }
    }
}

fn count_status(counts: &mut BTreeMap<String, usize>, status: impl Serialize) -> Result<()> {
    let value = serde_json::to_value(status)?;
    let name = value.as_str().context("unexpected ledger status shape")?;
    *counts.entry(name.to_owned()).or_default() += 1;
    Ok(())
}

/// Replay one caller-admitted local archive. The manifest and parent checks
/// bind the input to a local object set. The caller supplies network and
/// canonicality evidence separately when that evidence is needed.
pub fn replay(path: &Path) -> Result<Value> {
    let manifest_bytes = read_bounded(path, MAX_MANIFEST_BYTES)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    check_manifest(&manifest)?;
    let verified_through_slot = manifest
        .blocks
        .last()
        .context("archive range has no blocks")?
        .slot;
    let directory = path
        .parent()
        .context("archive manifest has no parent directory")?;
    decode_block(directory, &manifest.anchor).context("invalid archive anchor")?;

    let mut observations = Vec::new();
    let mut gaps = Vec::new();
    let mut transactions_scanned = 0;
    let mut candidates = 0;
    let mut measurement_count = 0;
    let mut measurement_kinds = BTreeMap::<&str, usize>::new();
    let mut earn_bound = 0;
    let mut earn_unbound = 0;
    let mut log_statuses = BTreeMap::<String, usize>::new();
    let mut execution_statuses = BTreeMap::<String, usize>::new();
    let mut kamino_return_statuses = BTreeMap::<String, usize>::new();
    let mut parser_errors = 0;
    for entry in &manifest.blocks {
        let decoded = decode_block(directory, entry)
            .with_context(|| format!("invalid archive block at slot {}", entry.slot))?;
        transactions_scanned += decoded.total_transactions;
        let transactions = decoded
            .block
            .transactions
            .context("archive block has no transactions")?;
        ensure!(
            transactions.len() == decoded.transaction_indices.len(),
            "archive transaction positions differ from rows"
        );
        for (transaction, tx_index) in transactions.into_iter().zip(decoded.transaction_indices) {
            if !touches(&transaction, |key| PROGRAMS.contains(&key)) {
                continue;
            }
            let has_jupiter = touches(&transaction, touches_jupiter);
            candidates += 1;
            let transaction = serde_json::to_value(transaction)?;
            let identity = || {
                json!({
                    "slot": entry.slot,
                    "blockhash": entry.block_hash,
                    "block_time": entry.block_time,
                    "block_wire_sha256": entry.wire_sha256,
                    "transaction_index": tx_index,
                    "signature": transaction.pointer("/transaction/signatures/0"),
                })
            };
            match parse_transaction(&transaction) {
                Ok(facts) => {
                    count_status(&mut execution_statuses, facts.execution_status)?;
                    count_status(&mut log_statuses, facts.log_status)?;
                    count_status(&mut kamino_return_statuses, facts.kamino_return_status)?;
                    let incomplete_jupiter = has_jupiter
                        && matches!(
                            facts.log_status,
                            LogStatus::Unavailable | LogStatus::Truncated | LogStatus::Invalid
                        );
                    let incomplete_kamino = matches!(
                        facts.kamino_return_status,
                        ReturnStatus::Unavailable | ReturnStatus::Invalid
                    );
                    if matches!(facts.execution_status, ExecutionStatus::Unknown)
                        || (matches!(facts.execution_status, ExecutionStatus::Succeeded)
                            && (incomplete_jupiter || incomplete_kamino))
                    {
                        let mut gap = identity();
                        gap["execution_status"] = serde_json::to_value(facts.execution_status)?;
                        gap["log_status"] = serde_json::to_value(facts.log_status)?;
                        gap["kamino_return_status"] =
                            serde_json::to_value(facts.kamino_return_status)?;
                        gaps.push(gap);
                    }
                    if !facts.measurements.is_empty() {
                        measurement_count += facts.measurements.len();
                        for fact in &facts.measurements {
                            let kind = match &fact.measurement {
                                LedgerMeasurement::JupiterEarnExchangePrices { .. } => {
                                    if fact.earn_market.is_some() {
                                        earn_bound += 1;
                                    } else {
                                        earn_unbound += 1;
                                    }
                                    "jupiter_earn_exchange_prices"
                                }
                                LedgerMeasurement::JupiterLiquidityExchangePrices { .. } => {
                                    "jupiter_liquidity_exchange_prices"
                                }
                                LedgerMeasurement::JupiterLiquidityOperationPrices { .. } => {
                                    "jupiter_liquidity_operation_prices"
                                }
                                LedgerMeasurement::KaminoReserveExchangePrice { .. } => {
                                    "kamino_reserve_exchange_price"
                                }
                            };
                            *measurement_kinds.entry(kind).or_default() += 1;
                        }
                        let mut row = identity();
                        row["measurements"] = serde_json::to_value(facts.measurements)?;
                        observations.push(row);
                    }
                }
                Err(_) => {
                    parser_errors += 1;
                    let mut gap = identity();
                    gap["status"] = Value::String("parser_error".into());
                    gaps.push(gap);
                }
            }
            ensure!(
                measurement_count + gaps.len() <= MAX_RESULT_ROWS,
                "archive replay output exceeds its row limit"
            );
        }
    }
    Ok(json!({
        "schema_version": 1,
        "input": "local_compact_v2_binary_archive",
        "manifest_sha256": digest(&manifest_bytes),
        "network_used": false,
        "rpc_calls": 0,
        "coverage": {
            "manifest_generation": manifest.generation,
            "manifest_verification_level": manifest.verification_level,
            "verified_parent_chain": true,
            "canonical_history_complete": false,
            "first_slot": manifest.first_slot,
            "last_slot": manifest.last_slot,
            "verified_through_slot": verified_through_slot,
            "anchor": {
                "slot": manifest.anchor.slot,
                "blockhash": manifest.anchor.block_hash,
                "wire_sha256": manifest.anchor.wire_sha256,
            },
            "omitted_slots": manifest.omitted_slots,
            "verified_bundles": manifest.blocks.len() + 1,
            "archive_bytes_including_anchor": manifest.output_bytes_including_anchor,
        },
        "counts": {
            "blocks_scanned": manifest.blocks.len(),
            "transactions_scanned": transactions_scanned,
            "candidates": candidates,
            "execution_statuses": execution_statuses,
            "log_statuses": log_statuses,
            "kamino_return_statuses": kamino_return_statuses,
            "parser_errors": parser_errors,
            "measurements": measurement_count,
            "measurements_by_kind": measurement_kinds,
            "jupiter_earn_market_bound": earn_bound,
            "jupiter_earn_market_unbound": earn_unbound,
            "gaps": gaps.len(),
        },
        "observations": observations,
        "gaps": gaps,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(slot: u64, parent_slot: u64, height: u64) -> Block {
        Block {
            slot,
            parent_slot,
            block_hash: format!("hash-{slot}"),
            previous_block_hash: format!("hash-{parent_slot}"),
            block_height: height,
            block_time: Some(1_700_000_000),
            file: format!("{slot}.bin"),
            wire_bytes: 5,
            wire_sha256: "digest".into(),
            transaction_count: 1,
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            schema: MANIFEST_SCHEMA.into(),
            generation: "fixture".into(),
            verification_level: "fixture".into(),
            first_slot: 12,
            last_slot: 14,
            omitted_slots: vec![13],
            output_bytes_including_anchor: 15,
            anchor: block(11, 10, 7),
            blocks: vec![block(12, 11, 8), block(14, 12, 9)],
        }
    }

    #[test]
    fn checks_the_parent_hash_and_height_across_an_omitted_slot() {
        let mut manifest = manifest();
        check_manifest(&manifest).unwrap();
        manifest.blocks[1].previous_block_hash = "different-parent".into();
        assert!(check_manifest(&manifest).is_err());
        manifest.blocks[1].previous_block_hash = manifest.blocks[0].block_hash.clone();
        manifest.blocks[1].block_height += 1;
        assert!(check_manifest(&manifest).is_err());
    }

    #[test]
    fn checks_omitted_slots_and_total_wire_bytes() {
        let mut manifest = manifest();
        manifest.omitted_slots.clear();
        assert!(check_manifest(&manifest).is_err());
        manifest.omitted_slots.push(13);
        manifest.output_bytes_including_anchor -= 1;
        assert!(check_manifest(&manifest).is_err());
    }

    #[test]
    fn rejects_a_block_path_outside_the_manifest_directory() {
        let mut block = block(14, 12, 9);
        block.file = "../14.bin".into();
        let error = decode_block(Path::new("."), &block).unwrap_err();
        assert_eq!(error.to_string(), "invalid archive block file name");
    }
}
