//! Typed native compact block conversion. The source schemas are pinned by
//! `../../../vendor/blockzilla/snapshot.json`; no RPC request is used by this library.
pub mod archive;
pub mod range_index;
mod wire;
use anyhow::{ensure, Context, Result};
use base64::{prelude::BASE64_STANDARD, Engine};
use blockzilla_archive_v2::*;
use blockzilla_compact::*;
use blockzilla_primitives::{
    bounded_historical_source_wincode_leb128_config, CompactPubkey, PubkeyResolver,
};
use ruzstd::decoding::StreamingDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use solana_message::MessageHeader;
use solana_transaction::versioned::TransactionVersion;
use solana_transaction_status_client_types::{
    option_serializer::OptionSerializer, EncodedTransaction, EncodedTransactionWithStatusMeta,
    ParsedAccount, ParsedAccountSource, ParsedInstruction, Reward, UiAddressTableLookup,
    UiCompiledInstruction, UiConfirmedBlock, UiInnerInstructions, UiInstruction, UiLoadedAddresses,
    UiMessage, UiParsedInstruction, UiParsedMessage, UiRawMessage, UiReturnDataEncoding,
    UiTransaction, UiTransactionConfig, UiTransactionReturnData, UiTransactionStatusMeta,
    UiTransactionTokenBalance,
};
use std::{collections::BTreeSet, io::Read};
extern crate wincode_blockzilla as wincode;
pub use wire::{
    parse_cursor, ChunkNotice, ChunkNoticeSource, Cursor, MAX_FRAME, MAX_META, MAX_PAYLOAD,
};

const MAX_BLOCK: usize = 64 * 1024 * 1024;
const MAX_TX: usize = 65_536;
const MAX_DECODED_CHUNK: usize = 128 * 1024 * 1024;
const MAX_LOG_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const FILES: &[&str] = &[
    "DIRECT-AGAVE-NORMALIZED.v1.json",
    "archive-v2-blocks.index",
    "archive-v2-blocks.zstd",
    "archive-v2-meta.wincode",
    "available-poh.wincode",
    "blockhash_registry.bin",
    "prev_blockhash_tail.bin",
    "registry.bin",
    "registry.mphf",
    "registry_counts.bin",
    "signatures.bin",
    "vote_hash_registry.bin",
];
const REQUIRED: &[&str] = &[
    "DIRECT-AGAVE-NORMALIZED.v1.json",
    "archive-v2-blocks.index",
    "archive-v2-blocks.zstd",
    "archive-v2-meta.wincode",
    "available-poh.wincode",
    "blockhash_registry.bin",
    "prev_blockhash_tail.bin",
    "registry.bin",
    "signatures.bin",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceIdentity {
    pub genesis_hash: String,
    pub node_identity: String,
    pub agave_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkCheckpoint {
    pub cursor: String,
    pub session: String,
    pub source: SourceIdentity,
    pub last_slot: u64,
    pub last_blockhash: String,
}
#[derive(Debug)]
pub struct DecodedBlock {
    pub slot: u64,
    /// Original bank positions, aligned with `block.transactions` after filtering.
    pub transaction_indices: Vec<u32>,
    /// Number of transaction rows in the admitted bank before projection.
    pub total_transactions: usize,
    pub block: UiConfirmedBlock,
}
#[derive(Debug)]
pub struct DecodedChunk {
    pub checkpoint: ChunkCheckpoint,
    pub first_slot: u64,
    pub first_parent_slot: u64,
    pub first_parent_blockhash: String,
    pub wire_bytes: usize,
    pub blocks: Vec<DecodedBlock>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BankTransaction {
    pub signature: String,
    pub index: u32,
    /// None means that no execution metadata was recorded.
    pub successful_execution: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DecodedBank {
    pub bank: blockzilla_bank_stream_v2::BankIdentity,
    pub block_height: u64,
    pub transactions: Vec<BankTransaction>,
    pub writable_accounts: BTreeSet<String>,
}

/// Decode the immutable processed payload. This result does not establish
/// confirmation; a separate matching native control must promote its bank.
fn admitted_bank(
    frame: &blockzilla_bank_stream_v2::Frame,
) -> Result<blockzilla_bank_stream_v2::Frame> {
    // Frame fields are public. Re-admit the bytes before semantic decoding.
    let admitted = blockzilla_bank_stream_v2::decode(frame.wire.clone())?;
    ensure!(
        admitted.sha256 == frame.sha256
            && admitted.cursor == frame.cursor
            && admitted.source == frame.source,
        "bank frame identity mismatch"
    );
    Ok(admitted)
}

fn decode_admitted_bank_block(
    admitted: &blockzilla_bank_stream_v2::Frame,
    options: &DecodeOptions,
) -> Result<DecodedBlock> {
    decode_admitted_bank_projection(admitted, options, None, None)
}

fn decode_admitted_bank_projection(
    admitted: &blockzilla_bank_stream_v2::Frame,
    options: &DecodeOptions,
    admission: Option<&mut AdmissionProjection>,
    financial: Option<&mut FinancialInterest>,
) -> Result<DecodedBlock> {
    let event = &admitted.envelope.event;
    ensure!(event.kind == "block", "control has no bank payload");
    let identities: Vec<BlockIdentity> =
        serde_json::from_value(admitted.envelope.marker["produced_blocks"].clone())?;
    ensure!(
        identities.len() == 1
            && identities[0].block_id == 0
            && identities[0].slot == event.bank.slot
            && identities[0].blockhash == event.bank.blockhash
            && identities[0].parent_slot == event.bank.parent_slot
            && identities[0].parent_blockhash == event.bank.parent_blockhash,
        "compact bank identity mismatch"
    );
    let metadata_len = u32_at(&admitted.wire, 8)? as usize;
    let payload = &admitted.wire[16 + metadata_len..];
    let object = |name: &str| -> Result<&[u8]> {
        let object = admitted
            .envelope
            .objects
            .iter()
            .find(|object| object.path == name)
            .with_context(|| format!("missing compact bank object: {name}"))?;
        let end = object
            .offset
            .checked_add(object.bytes)
            .context("object overflow")?;
        payload
            .get(object.offset..end)
            .context("object outside bank payload")
    };
    let mut blocks =
        decode_compact_projection(&object, &identities, options, admission, financial)?;
    let decoded = blocks.pop().context("missing compact bank")?;
    let block = &decoded.block;
    let block_height = event.block_height.context("missing native bank height")?;
    ensure!(
        block.block_height == Some(block_height),
        "native payload height mismatch"
    );
    Ok(decoded)
}

/// A financial projection still needs a matching root control before apply.
/// Empty compact token lists cannot prove a recorded empty list or account closure.
pub fn decode_bank_block(
    frame: &blockzilla_bank_stream_v2::Frame,
    options: &DecodeOptions,
) -> Result<DecodedBlock> {
    let admitted = admitted_bank(frame)?;
    let mut decoded = decode_admitted_bank_block(&admitted, options)?;
    normalize_bank_token_coverage(&mut decoded);
    Ok(decoded)
}

/// Keep every validation check, but create transaction UI values only for the
/// financial interest set. The caller must include every account and mint used
/// by its financial ingestion rules. Unknown metadata uses full projection.
/// A matching root control is still required before financial apply.
pub fn decode_bank_financial_block(
    frame: &blockzilla_bank_stream_v2::Frame,
    options: &DecodeOptions,
    interest: &mut FinancialInterest,
) -> Result<DecodedBlock> {
    interest.enforce_bound();
    let admitted = admitted_bank(frame)?;
    let mut decoded = decode_admitted_bank_projection(&admitted, options, None, Some(interest))?;
    normalize_bank_token_coverage(&mut decoded);
    Ok(decoded)
}

fn normalize_bank_token_coverage(decoded: &mut DecodedBlock) {
    for tx in decoded.block.transactions.iter_mut().flatten() {
        if let Some(meta) = &mut tx.meta {
            if matches!(&meta.pre_token_balances, OptionSerializer::Some(values) if values.is_empty())
            {
                meta.pre_token_balances = OptionSerializer::Skip;
            }
            if matches!(&meta.post_token_balances, OptionSerializer::Some(values) if values.is_empty())
            {
                meta.post_token_balances = OptionSerializer::Skip;
            }
        }
    }
}

pub fn decode_bank_payload(frame: &blockzilla_bank_stream_v2::Frame) -> Result<DecodedBank> {
    let admitted = admitted_bank(frame)?;
    let mut projection = AdmissionProjection::default();
    decode_admitted_bank_projection(
        &admitted,
        &DecodeOptions::default(),
        Some(&mut projection),
        None,
    )?;
    let mut writable_accounts = BTreeSet::new();
    for key in &projection.writable_accounts {
        let encoded = base58(key);
        serde_json::to_writer(&mut projection.output_budget, &encoded)
            .context("decoded admission output exceeds limit")?;
        writable_accounts.insert(encoded);
    }
    let decoded = DecodedBank {
        bank: admitted.envelope.event.bank.clone(),
        block_height: admitted
            .envelope
            .event
            .block_height
            .context("missing native bank height")?,
        transactions: projection.transactions,
        writable_accounts,
    };
    // This view has no instruction/log/token UI fields. Bound its actual output;
    // financial decoding separately keeps the full transaction UI output bound.
    serde_json::to_writer(&mut OutputBudget(0), &decoded)
        .context("decoded admission output exceeds limit")?;
    Ok(decoded)
}

#[cfg(test)]
fn decode_bank_payload_full_projection(
    frame: &blockzilla_bank_stream_v2::Frame,
) -> Result<DecodedBank> {
    let admitted = admitted_bank(frame)?;
    let event = &admitted.envelope.event;
    let block = decode_admitted_bank_block(&admitted, &DecodeOptions::default())?.block;
    let block_height = event.block_height.context("missing native bank height")?;
    let mut writable_accounts = BTreeSet::new();
    let mut transactions = Vec::new();
    for (index, tx) in block
        .transactions
        .context("missing bank transactions")?
        .into_iter()
        .enumerate()
    {
        let EncodedTransaction::Json(transaction) = &tx.transaction else {
            anyhow::bail!("unsupported bank transaction encoding");
        };
        let signature = transaction
            .signatures
            .first()
            .context("missing transaction signature")?
            .clone();
        let mut all_keys = Vec::new();
        match &transaction.message {
            UiMessage::Raw(message) => {
                let signed = message.header.num_required_signatures as usize;
                let signed_writable = signed - message.header.num_readonly_signed_accounts as usize;
                let unsigned_writable = message.account_keys.len()
                    - message.header.num_readonly_unsigned_accounts as usize;
                for (index, key) in message.account_keys.iter().enumerate() {
                    if (index < signed && index < signed_writable)
                        || (index >= signed && index < unsigned_writable)
                    {
                        writable_accounts.insert(key.clone());
                    }
                }
                all_keys.extend(message.account_keys.iter().cloned());
                if let Some(meta) = &tx.meta {
                    if let OptionSerializer::Some(loaded) = &meta.loaded_addresses {
                        writable_accounts.extend(loaded.writable.iter().cloned());
                        all_keys.extend(loaded.writable.iter().chain(&loaded.readonly).cloned());
                    }
                }
            }
            UiMessage::Parsed(message) => {
                writable_accounts.extend(
                    message
                        .account_keys
                        .iter()
                        .filter(|key| key.writable)
                        .map(|key| key.pubkey.clone()),
                );
                all_keys.extend(message.account_keys.iter().map(|key| key.pubkey.clone()));
            }
        }
        let (successful_execution, error) = match &tx.meta {
            Some(meta) => {
                for (index, (before, after)) in meta
                    .pre_balances
                    .iter()
                    .zip(&meta.post_balances)
                    .enumerate()
                {
                    if before != after {
                        writable_accounts.insert(
                            all_keys
                                .get(index)
                                .context("balance key unavailable")?
                                .clone(),
                        );
                    }
                }
                if let OptionSerializer::Some(rewards) = &meta.rewards {
                    writable_accounts.extend(rewards.iter().map(|reward| reward.pubkey.clone()));
                }
                (
                    Some(meta.err.is_none() && meta.status.is_ok()),
                    meta.err.as_ref().map(serde_json::to_string).transpose()?,
                )
            }
            None => (None, None),
        };
        transactions.push(BankTransaction {
            signature,
            index: u32::try_from(index)?,
            successful_execution,
            error,
        });
    }
    if let Some(rewards) = block.rewards {
        writable_accounts.extend(rewards.into_iter().map(|reward| reward.pubkey));
    }
    Ok(DecodedBank {
        bank: event.bank.clone(),
        block_height,
        transactions,
        writable_accounts,
    })
}
#[derive(Default)]
pub struct DecodeOptions {
    /// Successful transactions which touch these programs must have recorded
    /// inner instructions and complete logs before the checkpoint can advance.
    pub required_programs: Vec<String>,
}

pub const MAX_FINANCIAL_INTEREST_KEYS: usize = 65_536;

/// A bounded conservative financial interest set. Full is also the fallback
/// when the caller cannot parse a configured key or an interest set is too big.
#[derive(Clone, Debug)]
pub enum FinancialInterest {
    Full,
    Filtered {
        accounts: BTreeSet<[u8; 32]>,
        mints: BTreeSet<[u8; 32]>,
    },
}

impl FinancialInterest {
    pub fn from_raw(accounts: BTreeSet<[u8; 32]>, mints: BTreeSet<[u8; 32]>) -> Self {
        let mut interest = Self::Filtered { accounts, mints };
        interest.enforce_bound();
        interest
    }

    fn enforce_bound(&mut self) {
        if matches!(self, Self::Filtered { accounts, mints }
            if accounts.len().saturating_add(mints.len()) > MAX_FINANCIAL_INTEREST_KEYS)
        {
            *self = Self::Full;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
    path: String,
    offset: usize,
    bytes: usize,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: String,
    profile: String,
    evidence_included: bool,
    marker: Value,
    objects: Vec<Object>,
}
#[derive(Deserialize)]
struct BlockIdentity {
    block_id: u32,
    slot: u64,
    parent_slot: u64,
    blockhash: [u8; 32],
    parent_blockhash: [u8; 32],
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn base58(bytes: &[u8]) -> String {
    // Keys, blockhashes and signatures dominate native block projections. Their
    // fixed widths have a linear encoder; instruction data keeps generic Base58.
    if let Ok(bytes) = <&[u8; 32]>::try_from(bytes) {
        let mut output = [0; five8::BASE58_ENCODED_32_MAX_LEN];
        let len = five8::encode_32(bytes, &mut output) as usize;
        std::str::from_utf8(&output[..len])
            .expect("five8 emits Base58 ASCII")
            .to_owned()
    } else if let Ok(bytes) = <&[u8; 64]>::try_from(bytes) {
        let mut output = [0; five8::BASE58_ENCODED_64_MAX_LEN];
        let len = five8::encode_64(bytes, &mut output) as usize;
        std::str::from_utf8(&output[..len])
            .expect("five8 emits Base58 ASCII")
            .to_owned()
    } else {
        bs58::encode(bytes).into_string()
    }
}

/// Decode a complete chunk and verify it against the notice and durable parent.
/// No partial block result is returned after any failure.
pub fn decode_chunk(
    wire: &[u8],
    notice: &impl ChunkNoticeSource,
    after: &str,
    previous: Option<&ChunkCheckpoint>,
    options: &DecodeOptions,
) -> Result<DecodedChunk> {
    let notice = notice.to_chunk_notice();
    notice.validate(&notice.cursor)?;
    ensure!(
        wire.len() >= 16 && wire.len() <= MAX_FRAME && &wire[..8] == b"BZSTRM01",
        "invalid frame magic or size"
    );
    let meta = u32_at(wire, 8)? as usize;
    let size = u32_at(wire, 12)? as usize;
    ensure!(
        (1..=MAX_META).contains(&meta) && size <= MAX_PAYLOAD && wire.len() == 16 + meta + size,
        "incomplete or trailing frame"
    );
    ensure!(
        wire.len() == notice.bytes && digest(wire) == notice.sha256,
        "chunk digest or length mismatch"
    );
    let envelope: Envelope = serde_json::from_slice(&wire[16..16 + meta])?;
    ensure!(
        envelope.schema == "blockzilla-stream-envelope-v1"
            && envelope.profile == "native-compact-projection-v1"
            && !envelope.evidence_included,
        "unsupported feed profile"
    );
    let marker = &envelope.marker;
    ensure!(
        marker["schema"] == "hivezilla-direct-agave-compact-v1"
            && marker["state"] == "COMPLETE"
            && marker["source_profile"] == "agave-bank-callbacks-v1"
            && marker["finalized"] == true
            && marker["canonical"] == false
            && marker["publication_ready"] == false
            && marker["finality_basis"] == "operator-trusted-agave-rooted-bank-callback",
        "invalid feed provenance"
    );
    let source_field = |name: &str| -> Result<String> {
        let value = marker[name].as_str().context("missing source identity")?;
        ensure!(
            !value.is_empty() && value.len() <= 128,
            "invalid source identity"
        );
        Ok(value.into())
    };
    let session = source_field("session_id")?;
    let source = SourceIdentity {
        genesis_hash: source_field("genesis_hash")?,
        node_identity: source_field("node_identity")?,
        agave_revision: source_field("agave_revision")?,
    };
    let identities: Vec<BlockIdentity> = serde_json::from_value(marker["produced_blocks"].clone())?;
    ensure!(
        (1..=32).contains(&identities.len())
            && marker["blocks"].as_u64() == Some(identities.len() as u64),
        "invalid block count"
    );
    let first = &identities[0];
    let last = identities.last().context("empty chunk")?;
    ensure!(
        marker["start_slot"].as_u64() == Some(first.slot)
            && marker["end_slot_exclusive"].as_u64() == last.slot.checked_add(1)
            && marker["epoch"].as_u64() == Some(first.slot / 432_000)
            && first.slot / 432_000 == last.slot / 432_000,
        "invalid chunk slot range"
    );
    ensure!(
        first.slot == notice.start_slot
            && last.slot == notice.end_slot
            && notice.cursor == format!("{session}:{}:{}", last.slot, notice.sha256),
        "chunk notice mismatch"
    );
    for (index, identity) in identities.iter().enumerate() {
        ensure!(
            identity.slot <= SAFE_INTEGER
                && identity.parent_slot < identity.slot
                && identity.block_id as usize == index,
            "invalid block identity"
        );
        if index > 0 {
            let parent = &identities[index - 1];
            ensure!(
                identity.parent_slot == parent.slot
                    && identity.parent_blockhash == parent.blockhash,
                "parent chain gap inside chunk"
            );
        }
    }
    let cursor = parse_cursor(after)?;
    ensure!(
        cursor.session == session && first.slot > cursor.slot && first.parent_slot == cursor.slot,
        "capture session changed or parent chain gap"
    );
    if let Some(previous) = previous {
        ensure!(
            previous.cursor == after
                && previous.session == session
                && previous.source == source
                && previous.last_slot == first.parent_slot
                && previous.last_blockhash == base58(&first.parent_blockhash),
            "source identity changed or durable parent chain gap"
        );
    }
    let payload = &wire[16 + meta..];
    let files = marker["files"].as_array().context("missing marker files")?;
    ensure!(
        files.len() <= 32 && envelope.objects.len() <= FILES.len(),
        "too many objects"
    );
    let mut names = BTreeSet::new();
    let mut offset = 0usize;
    for object in &envelope.objects {
        ensure!(
            FILES.contains(&object.path.as_str())
                && names.insert(object.path.as_str())
                && object.offset == offset,
            "invalid object layout"
        );
        offset = offset
            .checked_add(object.bytes)
            .context("object overflow")?;
        ensure!(
            offset <= payload.len() && digest(&payload[object.offset..offset]) == object.sha256,
            "object digest mismatch"
        );
        let matching: Vec<_> = files
            .iter()
            .filter(|file| file["path"] == object.path)
            .collect();
        ensure!(
            matching.len() == 1
                && matching[0]["bytes"].as_u64() == Some(object.bytes as u64)
                && matching[0]["sha256"] == object.sha256,
            "marker object mismatch"
        );
    }
    ensure!(
        offset == payload.len()
            && REQUIRED.iter().all(|name| names.contains(name))
            && files
                .iter()
                .filter_map(|file| file["path"].as_str())
                .filter(|name| FILES.contains(name))
                .all(|name| names.contains(name)),
        "missing decode dependency"
    );
    let object = |name: &str| -> Result<&[u8]> {
        let row = envelope
            .objects
            .iter()
            .find(|row| row.path == name)
            .context("missing compact object")?;
        payload
            .get(row.offset..row.offset + row.bytes)
            .context("object outside payload")
    };
    let blocks = decode_compact_blocks(&object, &identities, options)?;
    Ok(DecodedChunk {
        checkpoint: ChunkCheckpoint {
            cursor: notice.cursor.clone(),
            session,
            source,
            last_slot: last.slot,
            last_blockhash: base58(&last.blockhash),
        },
        first_slot: first.slot,
        first_parent_slot: first.parent_slot,
        first_parent_blockhash: base58(&first.parent_blockhash),
        wire_bytes: wire.len(),
        blocks,
    })
}

fn decode_compact_blocks<'a>(
    object: &impl Fn(&str) -> Result<&'a [u8]>,
    identities: &[BlockIdentity],
    options: &DecodeOptions,
) -> Result<Vec<DecodedBlock>> {
    decode_compact_projection(object, identities, options, None, None)
}

fn decode_compact_projection<'a>(
    object: &impl Fn(&str) -> Result<&'a [u8]>,
    identities: &[BlockIdentity],
    options: &DecodeOptions,
    mut admission: Option<&mut AdmissionProjection>,
    mut financial: Option<&mut FinancialInterest>,
) -> Result<Vec<DecodedBlock>> {
    ensure!(
        admission.is_none() || financial.is_none(),
        "conflicting bank projection modes"
    );
    ensure!(
        admission.is_none() || options.required_programs.is_empty(),
        "admission cannot project required receipts"
    );
    let first = identities.first().context("empty bank identities")?;
    let blob = object("archive-v2-blocks.zstd")?;
    let (flags, rows) = index(object("archive-v2-blocks.index")?, blob.len())?;
    ensure!(
        rows.len() == identities.len(),
        "block identity count mismatch"
    );
    let registry = object("registry.bin")?;
    let hashes = object("blockhash_registry.bin")?;
    let tail = object("prev_blockhash_tail.bin")?;
    let signatures = object("signatures.bin")?;
    ensure!(
        registry.len().is_multiple_of(32)
            && signatures.len().is_multiple_of(64)
            && hashes.len() == rows.len() * 32
            && tail.len() == 40,
        "invalid dictionary layout"
    );
    ensure!(
        tail[..32] == first.parent_blockhash && u64_at(tail, 32)? == first.parent_slot,
        "parent tail mismatch"
    );
    let last_row = rows.last().context("empty index")?;
    ensure!(
        last_row.first_signature + last_row.signatures == signatures.len() / 64,
        "signature file count mismatch"
    );
    let required: BTreeSet<_> = options
        .required_programs
        .iter()
        .map(String::as_str)
        .collect();
    let required_raw = required
        .iter()
        .map(|program| {
            bs58::decode(program)
                .into_vec()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid required program key"))
        })
        .collect::<Result<BTreeSet<[u8; 32]>>>()?;
    let mut blocks = Vec::with_capacity(rows.len());
    let mut decoded_bytes = 0usize;
    let mut output_budget = OutputBudget(0);
    for (row, identity) in rows.iter().zip(identities) {
        ensure!(
            row.slot == identity.slot && row.block_id == identity.block_id,
            "index identity mismatch"
        );
        decoded_bytes = decoded_bytes
            .checked_add(row.uncompressed)
            .context("decoded size overflow")?;
        ensure!(
            decoded_bytes <= MAX_DECODED_CHUNK,
            "decoded chunk exceeds limit"
        );
        let input = &blob[row.offset..row.offset + row.compressed];
        let mut decoded = Vec::with_capacity(row.uncompressed.min(1_048_576));
        if flags == 2 {
            decoded.extend_from_slice(input);
        } else {
            let mut zstd = StreamingDecoder::new(input).context("zstd header")?;
            (&mut zstd)
                .take(row.uncompressed as u64 + 1)
                .read_to_end(&mut decoded)
                .context("zstd decode")?;
            ensure!(zstd.into_inner().is_empty(), "trailing zstd bytes");
        }
        ensure!(
            decoded.len() == row.uncompressed,
            "decoded block length mismatch"
        );
        let block =
            deserialize_archive_v2_hot_block_blob_borrowed_current_with_preallocation_limit::<
                1_048_576,
            >(&decoded)?;
        ensure!(
            block.header.slot == row.slot
                && block.header.parent_slot == identity.parent_slot
                && block.tx_count as usize == row.transactions
                && block.tx_rows_len() == row.transactions
                && block.header.blockhash_id == row.block_id
                && block.header.previous_blockhash_id == row.block_id.saturating_sub(1),
            "decoded block identity mismatch"
        );
        let start = row.block_id as usize * 32;
        ensure!(
            hashes.get(start..start + 32) == Some(identity.blockhash.as_slice()),
            "decoded blockhash mismatch"
        );
        if row.block_id > 0 {
            ensure!(
                hashes.get(start - 32..start) == Some(identity.parent_blockhash.as_slice()),
                "decoded parent hash mismatch"
            );
        }
        let mut transactions = if admission.is_some() {
            Vec::new()
        } else {
            Vec::with_capacity(row.transactions)
        };
        let mut transaction_indices = Vec::with_capacity(transactions.capacity());
        let mut skipped_writes = BTreeSet::new();
        let (mut message_at, mut metadata_at, mut signature_at) =
            (0usize, 0usize, row.first_signature * 64);
        for (tx_index, tx_row) in block.tx_rows().enumerate() {
            ensure!(
                tx_row.tx_index as usize == tx_index
                    && tx_row.reserved == [0; 3]
                    && tx_row.flags & !2047 == 0,
                "invalid transaction row"
            );
            ensure!(
                tx_row.flags
                    & (ARCHIVE_V2_TX_FLAG_TX_RAW_FALLBACK
                        | ARCHIVE_V2_TX_FLAG_METADATA_RAW_FALLBACK)
                    == 0,
                "raw fallback is unsupported"
            );
            ensure!(
                tx_row.message_offset as usize == message_at
                    && tx_row.metadata_offset as usize == metadata_at,
                "noncontiguous transaction regions"
            );
            message_at = message_at
                .checked_add(tx_row.message_len as usize)
                .context("message overflow")?;
            metadata_at = metadata_at
                .checked_add(tx_row.metadata_len as usize)
                .context("metadata overflow")?;
            let message: ArchiveV2HotMessagePayload = exact(
                block
                    .message_bytes
                    .get(tx_row.message_offset as usize..message_at)
                    .context("message outside block")?,
            )?;
            let metadata: Option<CompactMetaV1> = if tx_row.flags & ARCHIVE_V2_TX_FLAG_HAS_METADATA
                != 0
            {
                Some(exact(
                    block
                        .metadata_bytes
                        .get(tx_row.metadata_offset as usize..metadata_at)
                        .context("metadata outside block")?,
                )?)
            } else {
                ensure!(
                    tx_row.metadata_len == 0 && tx_row.flags & ARCHIVE_V2_TX_FLAG_HAS_ERROR == 0,
                    "missing metadata"
                );
                None
            };
            let signature_end = signature_at
                .checked_add(tx_row.signature_count as usize * 64)
                .context("signature overflow")?;
            let tx_signatures = signatures
                .get(signature_at..signature_end)
                .context("signature outside file")?;
            signature_at = signature_end;
            if let Some(admission) = admission.as_deref_mut() {
                let transaction = admit_transaction(
                    message,
                    metadata,
                    tx_row.flags,
                    tx_row.tx_index,
                    tx_signatures,
                    NativeDictionaries {
                        registry,
                        hashes,
                        tail,
                    },
                    &mut admission.writable_accounts,
                )?;
                serde_json::to_writer(&mut admission.output_budget, &transaction)
                    .context("decoded admission output exceeds limit")?;
                ensure!(
                    admission.writable_accounts.len() <= MAX_OUTPUT_BYTES / 32,
                    "decoded admission account output exceeds limit"
                );
                admission.transactions.push(transaction);
                continue;
            }
            if let Some(interest) = financial.as_deref_mut() {
                if !financial_transaction_selected(
                    &message,
                    metadata.as_ref(),
                    registry,
                    &required_raw,
                    interest,
                )? {
                    // The same native validator runs even when no transaction
                    // UI is needed. Reuse bounded scratch space for write keys.
                    skipped_writes.clear();
                    admit_transaction(
                        message,
                        metadata,
                        tx_row.flags,
                        tx_row.tx_index,
                        tx_signatures,
                        NativeDictionaries {
                            registry,
                            hashes,
                            tail,
                        },
                        &mut skipped_writes,
                    )?;
                    ensure!(
                        skipped_writes.len() <= MAX_OUTPUT_BYTES / 32,
                        "decoded admission account output exceeds limit"
                    );
                    continue;
                }
            }
            let transaction = convert_transaction(
                message,
                metadata,
                tx_row.flags,
                tx_row.signature_count,
                tx_signatures
                    .as_chunks::<64>()
                    .0
                    .iter()
                    .map(|value| base58(value))
                    .collect(),
                registry,
                hashes,
                tail,
                &required,
            )?;
            serde_json::to_writer(&mut output_budget, &transaction)
                .context("decoded transaction output exceeds limit")?;
            transaction_indices.push(tx_row.tx_index);
            transactions.push(transaction);
        }
        ensure!(
            message_at == block.message_bytes.len()
                && metadata_at == block.metadata_bytes.len()
                && signature_at == (row.first_signature + row.signatures) * 64,
            "unconsumed transaction region"
        );
        if let Some(admission) = admission.as_deref_mut() {
            for reward in block
                .header
                .rewards
                .iter()
                .flat_map(|rewards| &rewards.decoded)
            {
                validate_reward(reward, registry)?;
                admission
                    .writable_accounts
                    .insert(key_bytes(reward.pubkey, registry)?);
            }
            ensure!(
                admission.writable_accounts.len() <= MAX_OUTPUT_BYTES / 32,
                "decoded admission account output exceeds limit"
            );
        }
        let rewards = if admission.is_none() {
            block
                .header
                .rewards
                .as_ref()
                .map(|rewards| {
                    rewards
                        .decoded
                        .iter()
                        .map(|reward| convert_reward(reward, registry))
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
        } else {
            None
        };
        let block_time = block.header.block_time;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        ensure!(
            block_time.is_none_or(|time| time >= 0 && time as u64 <= now.saturating_add(5)),
            "block time is invalid or in the future"
        );
        // A successful required-program transaction needs its recorded chain time.
        if transactions
            .iter()
            .any(|tx| transaction_requires_receipts(tx, &required))
        {
            ensure!(
                block_time.is_some_and(|time| time >= 0),
                "required receipt block time is unavailable"
            );
        }
        blocks.push(DecodedBlock {
            slot: row.slot,
            transaction_indices,
            total_transactions: row.transactions,
            block: UiConfirmedBlock {
                previous_blockhash: base58(&identity.parent_blockhash),
                blockhash: base58(&identity.blockhash),
                parent_slot: identity.parent_slot,
                transactions: Some(transactions),
                signatures: None,
                rewards,
                num_reward_partitions: block
                    .header
                    .rewards
                    .as_ref()
                    .and_then(|rewards| rewards.num_partitions),
                block_time,
                block_height: block.header.block_height,
            },
        });
    }
    Ok(blocks)
}

fn financial_transaction_selected(
    message: &ArchiveV2HotMessagePayload,
    metadata: Option<&CompactMetaV1>,
    registry: &[u8],
    required: &BTreeSet<[u8; 32]>,
    interest: &mut FinancialInterest,
) -> Result<bool> {
    let FinancialInterest::Filtered { accounts, mints } = interest else {
        return Ok(true);
    };
    // No recorded execution metadata cannot establish unrelatedness. The full
    // converter retains unknown execution or rejects missing ALT/receipt data.
    let Some(metadata) = metadata else {
        return Ok(true);
    };
    let static_keys = match message {
        ArchiveV2HotMessagePayload::Legacy(message) => &message.account_keys,
        ArchiveV2HotMessagePayload::V0(message) => &message.account_keys,
        ArchiveV2HotMessagePayload::V1(message) => &message.account_keys,
    };
    let all_keys = static_keys
        .iter()
        .chain(&metadata.loaded_writable_addresses)
        .chain(&metadata.loaded_readonly_addresses)
        .map(|key| key_bytes(*key, registry))
        .collect::<Result<Vec<_>>>()?;
    let mut selected = all_keys
        .iter()
        .any(|key| accounts.contains(key) || required.contains(key));
    for balance in metadata
        .pre_token_balances
        .iter()
        .chain(&metadata.post_token_balances)
    {
        ensure!(balance.account_index <= 255, "invalid token balance index");
        let account = all_keys
            .get(balance.account_index as usize)
            .context("invalid token balance index")?;
        let mint = key_bytes(
            balance.mint.context("token balance mint unavailable")?,
            registry,
        )?;
        if mints.contains(&mint) {
            selected = true;
            accounts.insert(*account);
            if accounts.len().saturating_add(mints.len()) > MAX_FINANCIAL_INTEREST_KEYS {
                // A full fallback preserves coverage; never drop interest keys.
                *interest = FinancialInterest::Full;
                return Ok(true);
            }
        }
    }
    Ok(selected)
}

#[derive(Default)]
struct AdmissionProjection {
    transactions: Vec<BankTransaction>,
    writable_accounts: BTreeSet<[u8; 32]>,
    output_budget: OutputBudget,
}

struct NativeDictionaries<'a> {
    registry: &'a [u8],
    hashes: &'a [u8],
    tail: &'a [u8],
}

// Validate the same typed native data as the full UI projection. Instruction
// bytes are already exact bounded native values; their Base58 form is not needed
// to establish execution outcomes or conservative account invalidation.
fn admit_transaction(
    message: ArchiveV2HotMessagePayload,
    metadata: Option<CompactMetaV1>,
    flags: u32,
    index: u32,
    signatures: &[u8],
    dictionaries: NativeDictionaries<'_>,
    touched: &mut BTreeSet<[u8; 32]>,
) -> Result<BankTransaction> {
    let NativeDictionaries {
        registry,
        hashes,
        tail,
    } = dictionaries;
    let (header, keys, recent, instructions, lookups, v0) = match message {
        ArchiveV2HotMessagePayload::Legacy(m) => (
            m.header,
            m.account_keys,
            m.recent_blockhash,
            m.instructions,
            None,
            false,
        ),
        ArchiveV2HotMessagePayload::V0(m) => (
            m.header,
            m.account_keys,
            m.recent_blockhash,
            m.instructions,
            Some(m.address_table_lookups),
            true,
        ),
        ArchiveV2HotMessagePayload::V1(m) => (
            m.header,
            m.account_keys,
            m.recent_blockhash,
            m.instructions,
            None,
            false,
        ),
    };
    let signed = signatures.len() / 64;
    ensure!(
        signatures.len().is_multiple_of(64)
            && signed > 0
            && header.num_required_signatures as usize == signed
            && signed <= keys.len()
            && header.num_readonly_signed_accounts as usize <= signed
            && header.num_readonly_unsigned_accounts as usize <= keys.len() - signed,
        "invalid signature/account header"
    );
    let expected_loaded = lookups.as_ref().map_or((0, 0), |lookups| {
        (
            lookups
                .iter()
                .map(|lookup| lookup.writable_indexes.len())
                .sum::<usize>(),
            lookups
                .iter()
                .map(|lookup| lookup.readonly_indexes.len())
                .sum::<usize>(),
        )
    });
    ensure!(
        v0 == (flags & ARCHIVE_V2_TX_FLAG_MESSAGE_V0 != 0)
            && (expected_loaded != (0, 0))
                == (flags & ARCHIVE_V2_TX_FLAG_HAS_LOADED_ADDRESSES != 0),
        "message/loaded flag mismatch"
    );
    let mut all_keys = keys
        .iter()
        .map(|key| key_bytes(*key, registry))
        .collect::<Result<Vec<_>>>()?;
    if let Some(meta) = &metadata {
        ensure!(
            (
                meta.loaded_writable_addresses.len(),
                meta.loaded_readonly_addresses.len()
            ) == expected_loaded
                && meta.err.is_some() == (flags & ARCHIVE_V2_TX_FLAG_HAS_ERROR != 0)
                && meta.inner_instructions.is_some()
                    == (flags & ARCHIVE_V2_TX_FLAG_HAS_INNER_IX != 0)
                && meta.logs.is_some() == (flags & ARCHIVE_V2_TX_FLAG_HAS_LOGS != 0)
                && meta.return_data.is_some() == (flags & ARCHIVE_V2_TX_FLAG_HAS_RETURN_DATA != 0)
                && (!meta.pre_token_balances.is_empty() || !meta.post_token_balances.is_empty())
                    == (flags & ARCHIVE_V2_TX_FLAG_HAS_TOKEN_BALANCES != 0),
            "metadata flag/count mismatch"
        );
        for key in meta
            .loaded_writable_addresses
            .iter()
            .chain(&meta.loaded_readonly_addresses)
        {
            all_keys.push(key_bytes(*key, registry)?);
        }
        ensure!(
            meta.pre_balances.len() == all_keys.len() && meta.post_balances.len() == all_keys.len(),
            "balance account count mismatch"
        );
    } else {
        ensure!(expected_loaded == (0, 0), "loaded addresses unavailable");
    }
    ensure!(
        all_keys.len() <= 256 && instructions.len() <= MAX_TX,
        "transaction limit exceeded"
    );
    for lookup in lookups.iter().flatten() {
        key_bytes(lookup.account_key, registry)?;
    }
    match recent {
        OwnedCompactRecentBlockhash::Nonce(_) | OwnedCompactRecentBlockhash::Id(-1) => {}
        OwnedCompactRecentBlockhash::Id(id) if id >= 0 => {
            let start = (id as usize)
                .checked_mul(32)
                .context("recent hash offset overflow")?;
            hashes
                .get(start..start + 32)
                .context("recent hash outside dictionary")?;
        }
        OwnedCompactRecentBlockhash::Id(_) => {
            anyhow::bail!("unsupported native negative recent hash id")
        }
    }
    ensure!(tail.len() == 40, "parent tail mismatch");
    let mut parsed_vote = false;
    for instruction in &instructions {
        ensure!(
            (instruction.program_id_index as usize) < all_keys.len()
                && instruction
                    .accounts
                    .iter()
                    .all(|index| (*index as usize) < all_keys.len()),
            "invalid instruction index"
        );
        let program = all_keys[instruction.program_id_index as usize];
        match &instruction.data {
            ArchiveV2HotInstructionData::System(_) => ensure!(
                program == [0; 32],
                "compact system instruction has a different program"
            ),
            ArchiveV2HotInstructionData::ComputeBudget(_) => ensure!(
                program
                    == solana_pubkey::pubkey!("ComputeBudget111111111111111111111111111111")
                        .to_bytes(),
                "compact compute budget instruction has a different program"
            ),
            ArchiveV2HotInstructionData::VoteCompactUpdateVoteState(_)
            | ArchiveV2HotInstructionData::VoteCompactUpdateVoteStateSwitch { .. }
            | ArchiveV2HotInstructionData::VoteTowerSync(_)
            | ArchiveV2HotInstructionData::VoteTowerSyncSwitch { .. } => {
                parsed_vote = true;
                ensure!(
                    program
                        == solana_pubkey::pubkey!("Vote111111111111111111111111111111111111111")
                            .to_bytes(),
                    "compact vote instruction has a different program"
                );
            }
            _ => {}
        }
    }
    ensure!(
        parsed_vote == (flags & ARCHIVE_V2_TX_FLAG_HAS_COMPACT_VOTE_IX != 0),
        "compact vote flag mismatch"
    );
    let signed_writable = signed - header.num_readonly_signed_accounts as usize;
    let unsigned_writable = keys.len() - header.num_readonly_unsigned_accounts as usize;
    for (index, key) in all_keys.iter().enumerate() {
        if index < signed_writable
            || (index >= signed && index < unsigned_writable)
            || (index >= keys.len() && index < keys.len() + expected_loaded.0)
        {
            touched.insert(*key);
        }
    }
    let (successful_execution, error) = match metadata {
        None => (None, None),
        Some(meta) => {
            if let Some(inner) = &meta.inner_instructions {
                for group in inner {
                    ensure!(
                        (group.index as usize) < instructions.len(),
                        "invalid outer instruction index"
                    );
                    u8::try_from(group.index)?;
                    for instruction in &group.instructions {
                        ensure!(
                            (instruction.program_id_index as usize) < all_keys.len()
                                && instruction
                                    .accounts
                                    .iter()
                                    .all(|index| (*index as usize) < all_keys.len()),
                            "invalid inner instruction index"
                        );
                        u8::try_from(instruction.program_id_index)?;
                    }
                }
            }
            for balance in meta
                .pre_token_balances
                .iter()
                .chain(&meta.post_token_balances)
            {
                ensure!(
                    (balance.account_index as usize) < all_keys.len()
                        && balance.account_index <= 255,
                    "invalid token balance index"
                );
                key_bytes(
                    balance.mint.context("token balance mint unavailable")?,
                    registry,
                )?;
                if let Some(owner) = balance.owner {
                    key_bytes(owner, registry)?;
                }
                if let Some(program) = balance.program_id {
                    key_bytes(program, registry)?;
                }
            }
            for (index, (before, after)) in meta
                .pre_balances
                .iter()
                .zip(&meta.post_balances)
                .enumerate()
            {
                if before != after {
                    touched.insert(all_keys[index]);
                }
            }
            for reward in &meta.rewards {
                validate_reward(reward, registry)?;
                touched.insert(key_bytes(reward.pubkey, registry)?);
            }
            if let Some(data) = &meta.return_data {
                key_bytes(data.program_id, registry)?;
            }
            let error: Option<solana_transaction::TransactionError> = meta
                .err
                .as_ref()
                .map(|error| serde_json::from_value(serde_json::to_value(error)?))
                .transpose()?;
            // Keep the same bounded scalar/table/reference validation as full
            // projection, including known-program log variants and rendered size.
            if let Some(logs) = meta.logs {
                checked_logs(logs, registry)?;
            }
            (
                Some(error.is_none()),
                error.as_ref().map(serde_json::to_string).transpose()?,
            )
        }
    };
    Ok(BankTransaction {
        signature: base58(&signatures[..64]),
        index,
        successful_execution,
        error,
    })
}

fn validate_reward(reward: &CompactReward, registry: &[u8]) -> Result<()> {
    ensure!(
        (0..=4).contains(&reward.reward_type),
        "unsupported reward type"
    );
    key_bytes(reward.pubkey, registry)?;
    Ok(())
}

#[derive(Default)]
struct OutputBudget(usize);
impl std::io::Write for OutputBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_OUTPUT_BYTES.saturating_sub(self.0) {
            return Err(std::io::Error::other("decoded output exceeds limit"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn transaction_requires_receipts(
    tx: &EncodedTransactionWithStatusMeta,
    required: &BTreeSet<&str>,
) -> bool {
    if !tx
        .meta
        .as_ref()
        .is_some_and(|meta| meta.err.is_none() && meta.status.is_ok())
    {
        return false;
    }
    let loaded_required = tx
        .meta
        .as_ref()
        .is_some_and(|meta| match &meta.loaded_addresses {
            OptionSerializer::Some(loaded) => loaded
                .writable
                .iter()
                .chain(&loaded.readonly)
                .any(|key| required.contains(key.as_str())),
            _ => false,
        });
    match &tx.transaction {
        EncodedTransaction::Json(tx) => match &tx.message {
            UiMessage::Raw(message) => {
                message
                    .account_keys
                    .iter()
                    .any(|key| required.contains(key.as_str()))
                    || loaded_required
            }
            UiMessage::Parsed(message) => message
                .account_keys
                .iter()
                .any(|key| required.contains(key.pubkey.as_str())),
        },
        _ => false,
    }
}

// Keep the independently verified wire fields explicit at the conversion boundary.
#[allow(clippy::too_many_arguments)]
fn convert_transaction<R: KeyLookup + ?Sized>(
    message: ArchiveV2HotMessagePayload,
    mut metadata: Option<CompactMetaV1>,
    flags: u32,
    signature_count: u8,
    signatures: Vec<String>,
    registry: &R,
    hashes: &[u8],
    tail: &[u8],
    required: &BTreeSet<&str>,
) -> Result<EncodedTransactionWithStatusMeta> {
    let (header, keys, recent, instructions, lookups, config, version) = match message {
        ArchiveV2HotMessagePayload::Legacy(m) => (
            m.header,
            m.account_keys,
            m.recent_blockhash,
            m.instructions,
            None,
            None,
            TransactionVersion::LEGACY,
        ),
        ArchiveV2HotMessagePayload::V0(m) => (
            m.header,
            m.account_keys,
            m.recent_blockhash,
            m.instructions,
            Some(m.address_table_lookups),
            None,
            TransactionVersion::Number(0),
        ),
        ArchiveV2HotMessagePayload::V1(m) => (
            m.header,
            m.account_keys,
            m.recent_blockhash,
            m.instructions,
            None,
            Some(m.config),
            TransactionVersion::Number(1),
        ),
    };
    ensure!(
        header.num_required_signatures == signature_count
            && signature_count > 0
            && signature_count as usize <= keys.len()
            && header.num_readonly_signed_accounts <= signature_count
            && header.num_readonly_unsigned_accounts as usize
                <= keys.len() - signature_count as usize,
        "invalid signature/account header"
    );
    let expected_loaded = lookups.as_ref().map_or((0, 0), |lookups| {
        (
            lookups
                .iter()
                .map(|lookup| lookup.writable_indexes.len())
                .sum::<usize>(),
            lookups
                .iter()
                .map(|lookup| lookup.readonly_indexes.len())
                .sum::<usize>(),
        )
    });
    ensure!(
        (version == TransactionVersion::Number(0)) == (flags & ARCHIVE_V2_TX_FLAG_MESSAGE_V0 != 0)
            && (expected_loaded != (0, 0))
                == (flags & ARCHIVE_V2_TX_FLAG_HAS_LOADED_ADDRESSES != 0),
        "message/loaded flag mismatch"
    );
    let static_keys = keys
        .iter()
        .map(|key| resolve_key(*key, registry))
        .collect::<Result<Vec<_>>>()?;
    let mut all_keys = static_keys.clone();
    if let Some(meta) = &metadata {
        ensure!(
            (
                meta.loaded_writable_addresses.len(),
                meta.loaded_readonly_addresses.len()
            ) == expected_loaded
                && meta.err.is_some() == (flags & ARCHIVE_V2_TX_FLAG_HAS_ERROR != 0)
                && meta.inner_instructions.is_some()
                    == (flags & ARCHIVE_V2_TX_FLAG_HAS_INNER_IX != 0)
                && meta.logs.is_some() == (flags & ARCHIVE_V2_TX_FLAG_HAS_LOGS != 0)
                && meta.return_data.is_some() == (flags & ARCHIVE_V2_TX_FLAG_HAS_RETURN_DATA != 0)
                && (!meta.pre_token_balances.is_empty() || !meta.post_token_balances.is_empty())
                    == (flags & ARCHIVE_V2_TX_FLAG_HAS_TOKEN_BALANCES != 0),
            "metadata flag/count mismatch"
        );
        all_keys.extend(
            meta.loaded_writable_addresses
                .iter()
                .chain(&meta.loaded_readonly_addresses)
                .map(|key| resolve_key(*key, registry))
                .collect::<Result<Vec<_>>>()?,
        );
        ensure!(
            meta.pre_balances.len() == all_keys.len() && meta.post_balances.len() == all_keys.len(),
            "balance account count mismatch"
        );
    } else {
        ensure!(expected_loaded == (0, 0), "loaded addresses unavailable");
    }
    ensure!(
        all_keys.len() <= 256 && instructions.len() <= 65_536,
        "transaction limit exceeded"
    );
    let requires_receipts = all_keys.iter().any(|key| required.contains(key.as_str()));
    if requires_receipts {
        let meta = metadata
            .as_ref()
            .context("required program metadata unavailable")?;
        if meta.err.is_none() {
            ensure!(
                meta.inner_instructions.is_some() && meta.logs.is_some(),
                "required receipt coverage unavailable"
            );
            ensure!(
                !meta
                    .logs
                    .as_ref()
                    .unwrap()
                    .events
                    .iter()
                    .any(|event| matches!(event, LogEvent::LogTruncated)),
                "required receipt logs are truncated"
            );
        }
    }
    let recent = match recent {
        OwnedCompactRecentBlockhash::Nonce(hash) => base58(&hash),
        OwnedCompactRecentBlockhash::Id(id) if id >= 0 => {
            let start = id as usize * 32;
            base58(
                hashes
                    .get(start..start + 32)
                    .context("recent hash outside dictionary")?,
            )
        }
        OwnedCompactRecentBlockhash::Id(-1) => base58(&tail[..32]),
        OwnedCompactRecentBlockhash::Id(_) => {
            anyhow::bail!("unsupported native negative recent hash id")
        }
    };
    let ui_lookups = lookups
        .map(|lookups| {
            lookups
                .into_iter()
                .map(|lookup| {
                    Ok(UiAddressTableLookup {
                        account_key: resolve_key(lookup.account_key, registry)?,
                        writable_indexes: lookup.writable_indexes,
                        readonly_indexes: lookup.readonly_indexes,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let ui_config = config.map(|config| UiTransactionConfig {
        priority_fee: config.priority_fee,
        compute_unit_limit: config.compute_unit_limit,
        loaded_accounts_data_size_limit: config.loaded_accounts_data_size_limit,
        heap_size: config.heap_size,
    });
    let parsed = instructions.iter().any(|instruction| {
        matches!(
            instruction.data,
            ArchiveV2HotInstructionData::VoteCompactUpdateVoteState(_)
                | ArchiveV2HotInstructionData::VoteCompactUpdateVoteStateSwitch { .. }
                | ArchiveV2HotInstructionData::VoteTowerSync(_)
                | ArchiveV2HotInstructionData::VoteTowerSyncSwitch { .. }
        )
    });
    ensure!(
        parsed == (flags & ARCHIVE_V2_TX_FLAG_HAS_COMPACT_VOTE_IX != 0),
        "compact vote flag mismatch"
    );
    let mut ui_instructions = Vec::with_capacity(instructions.len());
    for instruction in &instructions {
        ensure!(
            (instruction.program_id_index as usize) < all_keys.len()
                && instruction
                    .accounts
                    .iter()
                    .all(|index| (*index as usize) < all_keys.len()),
            "invalid instruction index"
        );
        match &instruction.data {
            ArchiveV2HotInstructionData::System(_) => ensure!(
                all_keys[instruction.program_id_index as usize]
                    == "11111111111111111111111111111111",
                "compact system instruction has a different program"
            ),
            ArchiveV2HotInstructionData::ComputeBudget(_) => ensure!(
                all_keys[instruction.program_id_index as usize]
                    == "ComputeBudget111111111111111111111111111111",
                "compact compute budget instruction has a different program"
            ),
            _ => {}
        }
        if let Some(bytes) = instruction_bytes(&instruction.data)? {
            ui_instructions.push(UiInstruction::Compiled(UiCompiledInstruction {
                program_id_index: instruction.program_id_index,
                accounts: instruction.accounts.clone(),
                data: base58(&bytes),
                stack_height: None,
            }));
        } else {
            ensure!(
                all_keys[instruction.program_id_index as usize]
                    == "Vote111111111111111111111111111111111111111",
                "compact vote instruction has a different program"
            );
            ui_instructions.push(UiInstruction::Parsed(UiParsedInstruction::Parsed(
                ParsedInstruction {
                    program: "vote".into(),
                    program_id: all_keys[instruction.program_id_index as usize].clone(),
                    // This is a semantic compact value. No original wire form is claimed.
                    parsed: json!({"blockzillaCompactInstruction": instruction.data}),
                    stack_height: None,
                },
            )));
        }
    }
    let ui_message = if parsed {
        let signed_writable =
            signature_count as usize - header.num_readonly_signed_accounts as usize;
        let unsigned_writable = static_keys.len() - header.num_readonly_unsigned_accounts as usize;
        UiMessage::Parsed(UiParsedMessage {
            account_keys: all_keys
                .iter()
                .enumerate()
                .map(|(index, key)| ParsedAccount {
                    pubkey: key.clone(),
                    signer: index < signature_count as usize,
                    writable: if index < signature_count as usize {
                        index < signed_writable
                    } else if index < static_keys.len() {
                        index < unsigned_writable
                    } else {
                        index < static_keys.len() + expected_loaded.0
                    },
                    source: Some(if index < static_keys.len() {
                        ParsedAccountSource::Transaction
                    } else {
                        ParsedAccountSource::LookupTable
                    }),
                })
                .collect(),
            recent_blockhash: recent,
            instructions: ui_instructions,
            address_table_lookups: ui_lookups,
            transaction_config: ui_config,
        })
    } else {
        UiMessage::Raw(UiRawMessage {
            header: MessageHeader {
                num_required_signatures: header.num_required_signatures,
                num_readonly_signed_accounts: header.num_readonly_signed_accounts,
                num_readonly_unsigned_accounts: header.num_readonly_unsigned_accounts,
            },
            account_keys: static_keys,
            recent_blockhash: recent,
            instructions: ui_instructions
                .into_iter()
                .map(|instruction| match instruction {
                    UiInstruction::Compiled(instruction) => instruction,
                    _ => unreachable!("parsed choice checked"),
                })
                .collect(),
            address_table_lookups: ui_lookups,
            transaction_config: ui_config,
        })
    };
    let meta = metadata
        .take()
        .map(|meta| convert_metadata(meta, registry, all_keys.len(), instructions.len()))
        .transpose()?;
    Ok(EncodedTransactionWithStatusMeta {
        transaction: EncodedTransaction::Json(UiTransaction {
            signatures,
            message: ui_message,
        }),
        meta,
        version: Some(version),
    })
}

fn convert_metadata<R: KeyLookup + ?Sized>(
    meta: CompactMetaV1,
    registry: &R,
    accounts: usize,
    outer_instructions: usize,
) -> Result<UiTransactionStatusMeta> {
    let err = meta
        .err
        .as_ref()
        .map(|error| serde_json::from_value(serde_json::to_value(error)?))
        .transpose()?;
    let status = err.clone().map_or(Ok(()), Err);
    let inner = meta
        .inner_instructions
        .map(|inner| {
            inner
                .into_iter()
                .map(|group| {
                    ensure!(
                        (group.index as usize) < outer_instructions,
                        "invalid outer instruction index"
                    );
                    let instructions = group
                        .instructions
                        .into_iter()
                        .map(|instruction| {
                            ensure!(
                                (instruction.program_id_index as usize) < accounts
                                    && instruction
                                        .accounts
                                        .iter()
                                        .all(|index| (*index as usize) < accounts),
                                "invalid inner instruction index"
                            );
                            Ok(UiInstruction::Compiled(UiCompiledInstruction {
                                program_id_index: u8::try_from(instruction.program_id_index)?,
                                accounts: instruction.accounts,
                                data: base58(&instruction.data),
                                stack_height: instruction.stack_height,
                            }))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Ok(UiInnerInstructions {
                        index: u8::try_from(group.index)?,
                        instructions,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let logs = meta
        .logs
        .map(|logs| checked_logs(logs, registry))
        .transpose()?;
    let balances = |balances: Vec<CompactTokenBalance>| {
        balances
            .iter()
            .map(|balance| convert_token_balance(balance, registry, accounts))
            .collect::<Result<Vec<_>>>()
    };
    Ok(UiTransactionStatusMeta {
        err,
        status,
        fee: meta.fee,
        pre_balances: meta.pre_balances,
        post_balances: meta.post_balances,
        inner_instructions: inner.into(),
        log_messages: logs.into(),
        pre_token_balances: OptionSerializer::Some(balances(meta.pre_token_balances)?),
        post_token_balances: OptionSerializer::Some(balances(meta.post_token_balances)?),
        rewards: OptionSerializer::Some(
            meta.rewards
                .iter()
                .map(|reward| convert_reward(reward, registry))
                .collect::<Result<Vec<_>>>()?,
        ),
        loaded_addresses: OptionSerializer::Some(UiLoadedAddresses {
            writable: meta
                .loaded_writable_addresses
                .iter()
                .map(|key| resolve_key(*key, registry))
                .collect::<Result<Vec<_>>>()?,
            readonly: meta
                .loaded_readonly_addresses
                .iter()
                .map(|key| resolve_key(*key, registry))
                .collect::<Result<Vec<_>>>()?,
        }),
        return_data: OptionSerializer::or_skip(
            meta.return_data
                .map(|data| -> Result<_> {
                    Ok(UiTransactionReturnData {
                        program_id: resolve_key(data.program_id, registry)?,
                        data: (
                            BASE64_STANDARD.encode(data.data),
                            UiReturnDataEncoding::Base64,
                        ),
                    })
                })
                .transpose()?,
        ),
        compute_units_consumed: OptionSerializer::or_skip(meta.compute_units_consumed),
        cost_units: OptionSerializer::or_skip(meta.cost_units),
    })
}

fn resolve_key<R: KeyLookup + ?Sized>(key: CompactPubkey, registry: &R) -> Result<String> {
    Ok(base58(&key_bytes(key, registry)?))
}
/// Internal lookup supports native dense registries and bounded archive access maps.
trait KeyLookup: std::panic::RefUnwindSafe {
    fn lookup_key(&self, id: u32) -> Option<[u8; 32]>;
}
impl KeyLookup for [u8] {
    fn lookup_key(&self, id: u32) -> Option<[u8; 32]> {
        let start = (id.checked_sub(1)? as usize).checked_mul(32)?;
        self.get(start..start.checked_add(32)?)?.try_into().ok()
    }
}
impl<const N: usize> KeyLookup for [u8; N] {
    fn lookup_key(&self, id: u32) -> Option<[u8; 32]> {
        self.as_slice().lookup_key(id)
    }
}
fn key_bytes<R: KeyLookup + ?Sized>(key: CompactPubkey, registry: &R) -> Result<[u8; 32]> {
    match key {
        CompactPubkey::Raw(key) => Ok(key),
        CompactPubkey::Id(id) => {
            ensure!(id > 0, "zero registry id");
            registry
                .lookup_key(id)
                .context("registry id outside dictionary")
        }
    }
}
struct Dictionary<'a, R: KeyLookup + ?Sized>(&'a R);
impl<R: KeyLookup + ?Sized> PubkeyResolver for Dictionary<'_, R> {
    fn resolve_pubkey(&self, id: u32) -> Option<[u8; 32]> {
        self.0.lookup_key(id)
    }
}

fn checked_logs<R: KeyLookup + ?Sized>(
    mut logs: CompactLogStream,
    registry: &R,
) -> Result<Vec<String>> {
    logs.try_for_each_pubkey(|key| key_bytes(key, registry).map(|_| ()))?;
    let mut offset = 0usize;
    for length in &logs.strings.lengths {
        let end = offset
            .checked_add(*length as usize)
            .context("log string overflow")?;
        std::str::from_utf8(
            logs.strings
                .bytes
                .get(offset..end)
                .context("log string outside table")?,
        )?;
        offset = end;
    }
    ensure!(offset == logs.strings.bytes.len(), "unconsumed log strings");
    let chunks = logs.data.arrays.iter().try_fold(0usize, |sum, array| {
        sum.checked_add(array.chunk_count as usize)
            .context("log data overflow")
    })?;
    ensure!(
        chunks == logs.data.chunk_lengths.len(),
        "log data chunk count mismatch"
    );
    let data_bytes = logs
        .data
        .chunk_lengths
        .iter()
        .try_fold(0usize, |sum, length| {
            sum.checked_add(*length as usize)
                .context("log data overflow")
        })?;
    ensure!(
        data_bytes == logs.data.bytes.len()
            && logs.events.len() <= MAX_TX
            && logs.strings.bytes.len() <= MAX_LOG_BYTES
            && data_bytes <= MAX_LOG_BYTES,
        "log table size mismatch or limit exceeded"
    );
    let events = std::mem::take(&mut logs.events);
    let mut output = Vec::with_capacity(events.len());
    let mut bytes = 0usize;
    for event in events {
        logs.events.push(event);
        // Upstream renderers use table-indexing APIs. Invalid scalar string/data
        // IDs must fail closed, including IDs in known-program log variants.
        let rendered = std::panic::catch_unwind(|| render_logs(&logs, &Dictionary(registry)))
            .map_err(|_| anyhow::anyhow!("invalid compact log reference"))?;
        logs.events.clear();
        for line in rendered {
            bytes = bytes
                .checked_add(line.len())
                .context("rendered log size overflow")?;
            ensure!(bytes <= MAX_LOG_BYTES, "rendered logs exceed limit");
            output.push(line);
        }
    }
    Ok(output)
}

fn convert_token_balance<R: KeyLookup + ?Sized>(
    balance: &CompactTokenBalance,
    registry: &R,
    accounts: usize,
) -> Result<UiTransactionTokenBalance> {
    ensure!(
        (balance.account_index as usize) < accounts && balance.account_index <= 255,
        "invalid token balance index"
    );
    let amount = balance.amount.to_string();
    let decimals = balance.decimals as usize;
    let ui_amount_string = if decimals == 0 {
        amount.clone()
    } else {
        let digits = format!("{:0>width$}", amount, width = decimals + 1);
        let split = digits.len() - decimals;
        format!("{}.{}", &digits[..split], &digits[split..])
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };
    let mut value = json!({"accountIndex": balance.account_index,
        "mint": balance.mint.map(|key| resolve_key(key, registry)).transpose()?.context("token balance mint unavailable")?,
        "uiTokenAmount": {"amount": amount, "decimals": balance.decimals, "uiAmount": null, "uiAmountString": ui_amount_string}});
    if let Some(owner) = balance.owner {
        value["owner"] = json!(resolve_key(owner, registry)?);
    }
    if let Some(program) = balance.program_id {
        value["programId"] = json!(resolve_key(program, registry)?);
    }
    Ok(serde_json::from_value(value)?)
}
fn convert_reward<R: KeyLookup + ?Sized>(reward: &CompactReward, registry: &R) -> Result<Reward> {
    let kind = match reward.reward_type {
        0 => None,
        1 => Some("Fee"),
        2 => Some("Rent"),
        3 => Some("Staking"),
        4 => Some("Voting"),
        _ => anyhow::bail!("unsupported reward type"),
    };
    Ok(serde_json::from_value(
        json!({"pubkey": resolve_key(reward.pubkey, registry)?, "lamports": reward.lamports,
        "postBalance": reward.post_balance, "rewardType": kind, "commission": reward.commission}),
    )?)
}

/// Typed vote instructions retain semantic compact values, because TowerSync
/// can have multiple original wire forms. Other instructions keep exact bytes.
fn instruction_bytes(data: &ArchiveV2HotInstructionData) -> Result<Option<Vec<u8>>> {
    let bytes = match data {
        ArchiveV2HotInstructionData::Raw(bytes)
        | ArchiveV2HotInstructionData::UnknownSystem(bytes)
        | ArchiveV2HotInstructionData::UnknownVote(bytes) => bytes.clone(),
        ArchiveV2HotInstructionData::ComputeBudget(data) => {
            let mut bytes = Vec::with_capacity(9);
            match data {
                ArchiveV2ComputeBudgetInstructionData::Unused => bytes.push(0),
                ArchiveV2ComputeBudgetInstructionData::RequestHeapFrame(value) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                ArchiveV2ComputeBudgetInstructionData::SetComputeUnitLimit(value) => {
                    bytes.push(2);
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                ArchiveV2ComputeBudgetInstructionData::SetComputeUnitPrice(value) => {
                    bytes.push(3);
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                ArchiveV2ComputeBudgetInstructionData::SetLoadedAccountsDataSizeLimit(value) => {
                    bytes.push(4);
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
            bytes
        }
        ArchiveV2HotInstructionData::System(data) => system_instruction_bytes(data)?,
        _ => return Ok(None),
    };
    Ok(Some(bytes))
}

fn system_instruction_bytes(data: &ArchiveV2SystemInstructionData) -> Result<Vec<u8>> {
    use ArchiveV2SystemInstructionData as S;
    let mut bytes = Vec::with_capacity(96);
    let tag = match data {
        S::CreateAccount { .. } => 0u32,
        S::Assign { .. } => 1,
        S::Transfer { .. } => 2,
        S::CreateAccountWithSeed { .. } => 3,
        S::AdvanceNonceAccount => 4,
        S::WithdrawNonceAccount { .. } => 5,
        S::InitializeNonceAccount { .. } => 6,
        S::AuthorizeNonceAccount { .. } => 7,
        S::Allocate { .. } => 8,
        S::AllocateWithSeed { .. } => 9,
        S::AssignWithSeed { .. } => 10,
        S::TransferWithSeed { .. } => 11,
        S::UpgradeNonceAccount => 12,
        S::CreateAccountAllowPrefund { .. } => 13,
    };
    bytes.extend_from_slice(&tag.to_le_bytes());
    let seed_bytes = |bytes: &mut Vec<u8>, seed: &str| -> Result<()> {
        bytes.extend_from_slice(&u64::try_from(seed.len())?.to_le_bytes());
        bytes.extend_from_slice(seed.as_bytes());
        Ok(())
    };
    match data {
        S::CreateAccount {
            lamports,
            space,
            owner,
        }
        | S::CreateAccountAllowPrefund {
            lamports,
            space,
            owner,
        } => {
            bytes.extend_from_slice(&lamports.to_le_bytes());
            bytes.extend_from_slice(&space.to_le_bytes());
            bytes.extend_from_slice(owner);
        }
        S::Assign { owner } => bytes.extend_from_slice(owner),
        S::Transfer { lamports } | S::WithdrawNonceAccount { lamports } => {
            bytes.extend_from_slice(&lamports.to_le_bytes())
        }
        S::CreateAccountWithSeed {
            base,
            seed,
            lamports,
            space,
            owner,
        } => {
            bytes.extend_from_slice(base);
            seed_bytes(&mut bytes, seed)?;
            bytes.extend_from_slice(&lamports.to_le_bytes());
            bytes.extend_from_slice(&space.to_le_bytes());
            bytes.extend_from_slice(owner);
        }
        S::AdvanceNonceAccount | S::UpgradeNonceAccount => {}
        S::InitializeNonceAccount { authority } | S::AuthorizeNonceAccount { authority } => {
            bytes.extend_from_slice(authority)
        }
        S::Allocate { space } => bytes.extend_from_slice(&space.to_le_bytes()),
        S::AllocateWithSeed {
            base,
            seed,
            space,
            owner,
        } => {
            bytes.extend_from_slice(base);
            seed_bytes(&mut bytes, seed)?;
            bytes.extend_from_slice(&space.to_le_bytes());
            bytes.extend_from_slice(owner);
        }
        S::AssignWithSeed { base, seed, owner } => {
            bytes.extend_from_slice(base);
            seed_bytes(&mut bytes, seed)?;
            bytes.extend_from_slice(owner);
        }
        S::TransferWithSeed {
            lamports,
            from_seed,
            from_owner,
        } => {
            bytes.extend_from_slice(&lamports.to_le_bytes());
            seed_bytes(&mut bytes, from_seed)?;
            bytes.extend_from_slice(from_owner);
        }
    }
    Ok(bytes)
}

// Bounded hot index and exact schema helpers from the pinned native decoder.
struct IndexRow {
    block_id: u32,
    slot: u64,
    offset: usize,
    compressed: usize,
    uncompressed: usize,
    transactions: usize,
    first_tx: usize,
    first_signature: usize,
    signatures: usize,
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .context("truncated_index")?
            .try_into()?,
    ))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .context("truncated_index")?
            .try_into()?,
    ))
}
fn index(bytes: &[u8], blob_len: usize) -> Result<(u32, Vec<IndexRow>)> {
    ensure!(
        bytes.get(..8) == Some(b"BZV2HIX1"),
        "invalid_hot_index_magic"
    );
    ensure!(
        bytes.get(8..12) == Some(&[1, 0, 0, 0]),
        "unsupported_hot_index_version"
    );
    let n = usize::try_from(u64_at(bytes, 12)?)?;
    ensure!(
        (1..=32).contains(&n) && bytes.len() == 36 + n * 52,
        "invalid_hot_index_length"
    );
    ensure!(
        u64_at(bytes, 20)? == blob_len as u64,
        "hot_blob_length_mismatch"
    );
    let flags = u32_at(bytes, 32)?;
    ensure!(
        flags == 0 || flags == 2,
        "unsupported_zstd_dictionary_or_flags"
    );
    let mut rows = Vec::with_capacity(n);
    let (mut offset, mut tx, mut sig) = (0, 0, 0);
    for b in bytes[36..].as_chunks::<52>().0 {
        let r = IndexRow {
            block_id: u32_at(b, 0)?,
            slot: u64_at(b, 4)?,
            offset: usize::try_from(u64_at(b, 12)?)?,
            compressed: u32_at(b, 20)? as usize,
            uncompressed: u32_at(b, 24)? as usize,
            transactions: u32_at(b, 28)? as usize,
            first_tx: usize::try_from(u64_at(b, 32)?)?,
            first_signature: usize::try_from(u64_at(b, 40)?)?,
            signatures: u32_at(b, 48)? as usize,
        };
        ensure!(
            r.block_id as usize == rows.len()
                && r.offset == offset
                && r.first_tx == tx
                && r.first_signature == sig,
            "non_contiguous_hot_index"
        );
        ensure!(
            r.compressed > 0
                && r.uncompressed > 0
                && r.uncompressed <= MAX_BLOCK
                && r.transactions <= MAX_TX,
            "block_limit_exceeded"
        );
        offset = offset.checked_add(r.compressed).context("index_overflow")?;
        tx = tx.checked_add(r.transactions).context("index_overflow")?;
        sig = sig.checked_add(r.signatures).context("index_overflow")?;
        ensure!(offset <= blob_len, "hot_range_outside_blob");
        rows.push(r);
    }
    ensure!(offset == blob_len, "unindexed_hot_bytes");
    Ok((flags, rows))
}
fn exact<
    'a,
    T: wincode::SchemaRead<
        'a,
        blockzilla_primitives::BoundedHistoricalSourceWincodeLeb128Config<1_048_576>,
        Dst = T,
    >,
>(
    bytes: &'a [u8],
) -> Result<T> {
    Ok(wincode::config::deserialize_exact::<T, _>(
        bytes,
        bounded_historical_source_wincode_leb128_config::<1_048_576>(),
    )?)
}

#[cfg(any(test, feature = "test-fixtures"))]
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
pub mod tests {
    use super::*;

    #[test]
    fn fixed_width_base58_matches_generic_encoding() {
        for length in [32, 64] {
            for zero_prefix in 0..=length {
                let mut bytes = vec![255; length];
                bytes[..zero_prefix].fill(0);
                assert_eq!(base58(&bytes), bs58::encode(&bytes).into_string());
            }
            let mut seed = 0x8f16_48ab_c9de_7325u64;
            for _ in 0..256 {
                let bytes: Vec<_> = (0..length)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        seed as u8
                    })
                    .collect();
                assert_eq!(base58(&bytes), bs58::encode(&bytes).into_string());
            }
        }
    }

    #[test]
    fn arbitrary_width_base58_preserves_generic_encoding() {
        for length in [0, 1, 31, 33, 63, 65, 128, 1024] {
            let bytes: Vec<_> = (0..length).map(|index| (index * 73) as u8).collect();
            assert_eq!(base58(&bytes), bs58::encode(&bytes).into_string());
        }
    }
    use blockzilla_primitives::wincode_leb128_config;

    pub fn fixture(
        version: u8,
        failed: bool,
        compressed: bool,
        time: Option<i64>,
        edit: impl FnOnce(&mut CompactMetaV1),
    ) -> (Vec<u8>, ChunkNotice, String) {
        fixture_with_instruction_data(version, failed, compressed, time, vec![1, 2, 3], edit)
    }

    pub fn fixture_with_instruction_data(
        version: u8,
        failed: bool,
        compressed: bool,
        time: Option<i64>,
        data: Vec<u8>,
        edit: impl FnOnce(&mut CompactMetaV1),
    ) -> (Vec<u8>, ChunkNotice, String) {
        let mut logs = CompactLogStream {
            events: vec![
                LogEvent::Invoke {
                    program: CompactPubkey::Id(1),
                    depth: 1,
                },
                LogEvent::Data { data: 0 },
                LogEvent::Success {
                    program: CompactPubkey::Id(1),
                },
            ],
            strings: Default::default(),
            data: Default::default(),
        };
        logs.data
            .push_base64_chunks("AQID", &mut Vec::new())
            .unwrap();
        let loaded = version == 0 || version == 2;
        let mut meta = CompactMetaV1 {
            err: failed.then_some(CompactTransactionError::AccountNotFound),
            fee: 5000,
            pre_balances: vec![u64::MAX; if loaded { 3 } else { 2 }],
            post_balances: vec![u64::MAX - 1; if loaded { 3 } else { 2 }],
            inner_instructions: Some(vec![CompactInnerInstructions {
                index: 0,
                instructions: vec![CompactInnerInstruction {
                    program_id_index: if loaded { 2 } else { 1 },
                    accounts: vec![0],
                    data: vec![9, 8],
                    stack_height: Some(2),
                }],
            }]),
            logs: Some(logs),
            pre_token_balances: vec![],
            post_token_balances: vec![CompactTokenBalance {
                account_index: 0,
                mint: Some(CompactPubkey::Id(1)),
                owner: Some(CompactPubkey::Raw([1; 32])),
                program_id: Some(CompactPubkey::Raw([6; 32])),
                amount: u64::MAX,
                decimals: 9,
            }],
            rewards: vec![CompactReward {
                pubkey: CompactPubkey::Raw([1; 32]),
                lamports: 2,
                post_balance: u64::MAX,
                reward_type: 1,
                commission: None,
            }],
            loaded_writable_addresses: if version == 2 {
                vec![CompactPubkey::Id(2)]
            } else {
                vec![]
            },
            loaded_readonly_addresses: if version == 0 {
                vec![CompactPubkey::Id(2)]
            } else {
                vec![]
            },
            return_data: Some(CompactReturnData {
                program_id: CompactPubkey::Id(1),
                data: vec![1, 2, 3],
            }),
            compute_units_consumed: Some(12),
            cost_units: Some(20),
        };
        edit(&mut meta);
        // Variant 2 records an unchanged loaded writable account.
        if version == 2 {
            meta.post_balances[2] = meta.pre_balances[2];
        }
        let header = CompactMessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 1,
        };
        let keys = vec![CompactPubkey::Raw([1; 32]), CompactPubkey::Id(1)];
        let instruction = ArchiveV2HotInstruction {
            program_id_index: 1,
            accounts: vec![0],
            data: ArchiveV2HotInstructionData::Raw(data),
        };
        let message = match version {
            0 | 2 => ArchiveV2HotMessagePayload::V0(ArchiveV2HotV0Message {
                header,
                account_keys: keys,
                recent_blockhash: OwnedCompactRecentBlockhash::Nonce([3; 32]),
                instructions: vec![instruction],
                address_table_lookups: vec![OwnedCompactAddressTableLookup {
                    account_key: CompactPubkey::Raw([4; 32]),
                    writable_indexes: if version == 2 { vec![0] } else { vec![] },
                    readonly_indexes: if version == 0 { vec![0] } else { vec![] },
                }],
            }),
            1 => ArchiveV2HotMessagePayload::V1(ArchiveV2HotV1Message {
                header,
                account_keys: keys,
                recent_blockhash: OwnedCompactRecentBlockhash::Nonce([3; 32]),
                instructions: vec![instruction],
                config: CompactTransactionConfig {
                    priority_fee: Some(u64::MAX),
                    compute_unit_limit: Some(123),
                    loaded_accounts_data_size_limit: Some(456),
                    heap_size: Some(32768),
                },
            }),
            _ => ArchiveV2HotMessagePayload::Legacy(ArchiveV2HotLegacyMessage {
                header,
                account_keys: keys,
                recent_blockhash: OwnedCompactRecentBlockhash::Nonce([3; 32]),
                instructions: vec![instruction],
            }),
        };
        let message_bytes = wincode::config::serialize(&message, wincode_leb128_config()).unwrap();
        let metadata_bytes = wincode::config::serialize(&meta, wincode_leb128_config()).unwrap();
        let mut flags = ARCHIVE_V2_TX_FLAG_HAS_METADATA;
        if !meta.pre_token_balances.is_empty() || !meta.post_token_balances.is_empty() {
            flags |= ARCHIVE_V2_TX_FLAG_HAS_TOKEN_BALANCES;
        }
        if meta.inner_instructions.is_some() {
            flags |= ARCHIVE_V2_TX_FLAG_HAS_INNER_IX;
        }
        if meta.logs.is_some() {
            flags |= ARCHIVE_V2_TX_FLAG_HAS_LOGS;
        }
        if meta.return_data.is_some() {
            flags |= ARCHIVE_V2_TX_FLAG_HAS_RETURN_DATA;
        }
        if failed {
            flags |= ARCHIVE_V2_TX_FLAG_HAS_ERROR;
        }
        if loaded {
            flags |= ARCHIVE_V2_TX_FLAG_MESSAGE_V0 | ARCHIVE_V2_TX_FLAG_HAS_LOADED_ADDRESSES;
        }
        let block = ArchiveV2HotBlockBlob {
            header: ArchiveV2HotBlockHeader {
                slot: 100,
                parent_slot: 99,
                blockhash_id: 0,
                previous_blockhash_id: 0,
                block_time: time,
                block_height: Some(321),
                rewards: None,
            },
            tx_count: 1,
            tx_rows: vec![ArchiveV2HotTxRow {
                tx_index: 0,
                flags,
                message_offset: 0,
                message_len: message_bytes.len() as u32,
                metadata_offset: 0,
                metadata_len: metadata_bytes.len() as u32,
                signature_count: 1,
                reserved: [0; 3],
            }],
            message_bytes,
            metadata_bytes,
        };
        let raw = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
        let blob = if compressed {
            ruzstd::encoding::compress_to_vec(
                raw.as_slice(),
                ruzstd::encoding::CompressionLevel::Fastest,
            )
        } else {
            raw.clone()
        };
        let mut index = b"BZV2HIX1".to_vec();
        index.extend([1, 0, 0, 0]);
        index.extend(1u64.to_le_bytes());
        index.extend((blob.len() as u64).to_le_bytes());
        index.extend(0i32.to_le_bytes());
        index.extend((if compressed { 0u32 } else { 2u32 }).to_le_bytes());
        index.extend(0u32.to_le_bytes());
        index.extend(100u64.to_le_bytes());
        index.extend(0u64.to_le_bytes());
        index.extend((blob.len() as u32).to_le_bytes());
        index.extend((raw.len() as u32).to_le_bytes());
        index.extend(1u32.to_le_bytes());
        index.extend(0u64.to_le_bytes());
        index.extend(0u64.to_le_bytes());
        index.extend(1u32.to_le_bytes());
        let files = vec![
            ("DIRECT-AGAVE-NORMALIZED.v1.json", b"{}".to_vec()),
            ("archive-v2-blocks.index", index),
            ("archive-v2-blocks.zstd", blob),
            ("archive-v2-meta.wincode", vec![42]),
            ("available-poh.wincode", vec![42]),
            ("registry.bin", [[7; 32], [8; 32]].concat()),
            ("blockhash_registry.bin", vec![2; 32]),
            (
                "prev_blockhash_tail.bin",
                [vec![3; 32], 99u64.to_le_bytes().to_vec()].concat(),
            ),
            ("signatures.bin", vec![9; 64]),
        ];
        let mut objects = vec![];
        let mut marker_files = vec![];
        let mut payload = vec![];
        for (path, bytes) in files {
            let hash = digest(&bytes);
            objects.push(
                json!({"path":path,"offset":payload.len(),"bytes":bytes.len(),"sha256":hash}),
            );
            marker_files.push(json!({"path":path,"bytes":bytes.len(),"sha256":hash}));
            payload.extend(bytes);
        }
        let marker = json!({"schema":"hivezilla-direct-agave-compact-v1","state":"COMPLETE","source_profile":"agave-bank-callbacks-v1",
            "finalized":true,"canonical":false,"publication_ready":false,"finality_basis":"operator-trusted-agave-rooted-bank-callback",
            "session_id":"test","genesis_hash":"genesis","node_identity":"node","agave_revision":"revision","epoch":0,
            "start_slot":100,"end_slot_exclusive":101,"blocks":1,"files":marker_files,
            "produced_blocks":[{"slot":100,"block_id":0,"parent_slot":99,"blockhash":vec![2;32],"parent_blockhash":vec![3;32]}]});
        let envelope = serde_json::to_vec(&json!({"schema":"blockzilla-stream-envelope-v1","profile":"native-compact-projection-v1",
            "evidence_included":false,"marker":marker,"objects":objects})).unwrap();
        let mut wire = b"BZSTRM01".to_vec();
        wire.extend((envelope.len() as u32).to_le_bytes());
        wire.extend((payload.len() as u32).to_le_bytes());
        wire.extend(envelope);
        wire.extend(payload);
        let hash = digest(&wire);
        let notice = ChunkNotice {
            cursor: format!("test:100:{hash}"),
            sha256: hash.clone(),
            bytes: wire.len(),
            start_slot: 100,
            end_slot: 100,
            url: format!("/v1/chunks/{hash}"),
        };
        (wire, notice, "test:99:-".into())
    }
    fn required() -> DecodeOptions {
        DecodeOptions {
            required_programs: vec![base58(&[7; 32])],
        }
    }

    pub fn bank_fixture(version: u8, failed: bool) -> blockzilla_bank_stream_v2::Frame {
        bank_fixture_with_meta(version, failed, |_| {})
    }

    pub fn bank_fixture_with_meta(
        version: u8,
        failed: bool,
        edit: impl FnOnce(&mut CompactMetaV1),
    ) -> blockzilla_bank_stream_v2::Frame {
        let (wire, _, _) = fixture(version, failed, false, Some(1_700_000_000), edit);
        bank_fixture_from_wire(wire)
    }

    /// Rebase an uncompressed synthetic one-bank fixture, including all compact
    /// objects and digests. This is not a production frame rewrite operation.
    pub fn rebase_bank_fixture(
        frame: &blockzilla_bank_stream_v2::Frame,
        bank: blockzilla_bank_stream_v2::BankIdentity,
        block_height: u64,
        sequence: u64,
        signature_byte: u8,
    ) -> blockzilla_bank_stream_v2::Frame {
        let metadata_len = u32_at(&frame.wire, 8).unwrap() as usize;
        let body = &frame.wire[16 + metadata_len..];
        let mut objects: Vec<_> = frame
            .envelope
            .objects
            .iter()
            .map(|object| {
                (
                    object.path.clone(),
                    body[object.offset..object.offset + object.bytes].to_vec(),
                )
            })
            .collect();
        let (_, hot) = objects
            .iter_mut()
            .find(|(name, _)| name == "archive-v2-blocks.zstd")
            .unwrap();
        let mut block: ArchiveV2HotBlockBlob = exact(hot).unwrap();
        block.header.slot = bank.slot;
        block.header.parent_slot = bank.parent_slot;
        block.header.block_height = Some(block_height);
        *hot = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
        let blob_len = hot.len();
        let (_, index) = objects
            .iter_mut()
            .find(|(name, _)| name == "archive-v2-blocks.index")
            .unwrap();
        assert_eq!(
            u32_at(index, 32).unwrap(),
            2,
            "rebase requires an uncompressed test fixture"
        );
        index[20..28].copy_from_slice(&(blob_len as u64).to_le_bytes());
        index[40..48].copy_from_slice(&bank.slot.to_le_bytes());
        index[56..60].copy_from_slice(&(blob_len as u32).to_le_bytes());
        index[60..64].copy_from_slice(&(blob_len as u32).to_le_bytes());
        *objects
            .iter_mut()
            .find(|(name, _)| name == "blockhash_registry.bin")
            .map(|(_, bytes)| bytes)
            .unwrap() = bank.blockhash.to_vec();
        *objects
            .iter_mut()
            .find(|(name, _)| name == "prev_blockhash_tail.bin")
            .map(|(_, bytes)| bytes)
            .unwrap() = [
            bank.parent_blockhash.as_slice(),
            &bank.parent_slot.to_le_bytes(),
        ]
        .concat();
        objects
            .iter_mut()
            .find(|(name, _)| name == "signatures.bin")
            .unwrap()
            .1
            .fill(signature_byte);
        let mut envelope = frame.envelope.clone();
        envelope.marker["start_slot"] = bank.slot.into();
        envelope.marker["end_slot_exclusive"] = (bank.slot + 1).into();
        let identity = &mut envelope.marker["produced_blocks"][0];
        identity["slot"] = bank.slot.into();
        identity["bank_id"] = bank.bank_id.into();
        identity["parent_slot"] = bank.parent_slot.into();
        identity["blockhash"] = serde_json::to_value(bank.blockhash).unwrap();
        identity["parent_blockhash"] = serde_json::to_value(bank.parent_blockhash).unwrap();
        let mut payload = Vec::new();
        for (object, (_, bytes)) in envelope.objects.iter_mut().zip(objects) {
            object.offset = payload.len();
            object.bytes = bytes.len();
            object.sha256 = digest(&bytes);
            payload.extend(bytes);
        }
        envelope.marker["files"] = serde_json::to_value(envelope.objects.iter().map(|object|
            json!({"path":object.path,"bytes":object.bytes,"sha256":object.sha256})).collect::<Vec<_>>()).unwrap();
        envelope.event.bank = bank;
        envelope.event.block_height = Some(block_height);
        envelope.event.sequence = sequence;
        envelope.event.source_sequence = sequence;
        envelope.event.payload_sha256 = digest(&serde_json::to_vec(&envelope.marker).unwrap());
        blockzilla_bank_stream_v2::encode(envelope, &payload).unwrap()
    }

    #[test]
    fn rebased_bank_fixture_has_matching_compact_parent_height_and_signature() {
        let bank = blockzilla_bank_stream_v2::BankIdentity {
            slot: 101,
            bank_id: 5_000_000_001,
            blockhash: [5; 32],
            parent_slot: 100,
            parent_blockhash: [2; 32],
        };
        let frame = rebase_bank_fixture(&bank_fixture(1, false), bank.clone(), 322, 3, 10);
        let decoded = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
        assert_eq!(frame.envelope.event.bank, bank);
        assert_eq!(decoded.slot, 101);
        assert_eq!(decoded.block.parent_slot, 100);
        assert_eq!(decoded.block.blockhash, base58(&[5; 32]));
        assert_eq!(decoded.block.previous_blockhash, base58(&[2; 32]));
        assert_eq!(decoded.block.block_height, Some(322));
        let EncodedTransaction::Json(tx) = &decoded.block.transactions.unwrap()[0].transaction
        else {
            panic!("typed projection")
        };
        assert_eq!(tx.signatures, vec![base58(&[10; 64])]);
    }

    /// Build a synthetic target transaction after zero to sixteen unrelated rows.
    /// Log strings are kept exact for dependent parser and pipeline tests.
    pub fn bank_fixture_with_program_logs(
        program: [u8; 32],
        failed: bool,
        logs: Option<Vec<String>>,
        transaction_index: u32,
        omit_metadata: bool,
    ) -> blockzilla_bank_stream_v2::Frame {
        assert!(transaction_index <= 16, "synthetic fixture row limit");
        let target = bank_fixture_with_meta(1, failed, |meta| {
            meta.logs = logs.map(|lines| {
                let mut stream = CompactLogStream {
                    events: Vec::new(),
                    strings: Default::default(),
                    data: Default::default(),
                };
                for line in lines {
                    let text = stream.strings.push(&line);
                    stream.events.push(LogEvent::Plain { text });
                }
                stream
            });
        });
        let target = rewrite_bank_object(&target, "archive-v2-blocks.zstd", |bytes| {
            let mut block: ArchiveV2HotBlockBlob = exact(bytes).unwrap();
            let mut native: ArchiveV2HotMessagePayload = exact(&block.message_bytes).unwrap();
            let ArchiveV2HotMessagePayload::V1(message) = &mut native else {
                panic!("v1 fixture")
            };
            message.account_keys[1] = CompactPubkey::Raw(program);
            block.message_bytes =
                wincode::config::serialize(&native, wincode_leb128_config()).unwrap();
            block.tx_rows[0].message_len = block.message_bytes.len() as u32;
            if omit_metadata {
                block.tx_rows[0].flags = 0;
                block.tx_rows[0].metadata_len = 0;
                block.metadata_bytes.clear();
            }
            *bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
        });
        if transaction_index == 0 {
            return target;
        }
        let mut rows: Vec<_> = (0..transaction_index)
            .map(|_| bank_fixture(1, false))
            .collect();
        rows.push(target);
        join_bank_transactions(&rows)
    }

    fn bank_fixture_from_wire(wire: Vec<u8>) -> blockzilla_bank_stream_v2::Frame {
        let meta_len = u32_at(&wire, 8).unwrap() as usize;
        let envelope: Envelope = serde_json::from_slice(&wire[16..16 + meta_len]).unwrap();
        let payload = &wire[16 + meta_len..];
        let mut marker = envelope.marker;
        marker["schema"] = "hivezilla-direct-agave-processed-compact-v1".into();
        marker["finalized"] = false.into();
        marker["genesis_hash"] = base58(&[11; 32]).into();
        marker["node_identity"] = base58(&[12; 32]).into();
        marker["produced_blocks"][0]["bank_id"] = 5_000_000_000u64.into();
        let bank = blockzilla_bank_stream_v2::BankIdentity {
            slot: 100,
            bank_id: 5_000_000_000,
            blockhash: [2; 32],
            parent_slot: 99,
            parent_blockhash: [3; 32],
        };
        let event = blockzilla_bank_stream_v2::BankEvent {
            sequence: 1,
            source_sequence: 1,
            kind: "block".into(),
            bank,
            payload_sha256: digest(&serde_json::to_vec(&marker).unwrap()),
            payload_frame_sha256: None,
            commitment: Some("processed".into()),
            confirmation_basis: Some("operator-trusted-agave-processed-bank-callback".into()),
            block_height: Some(321),
            recent_blockhash: None,
            observed_at_unix_ms: 1_700_000_000_000,
            reason: None,
        };
        blockzilla_bank_stream_v2::encode(
            blockzilla_bank_stream_v2::Envelope {
                schema: blockzilla_bank_stream_v2::ENVELOPE.into(),
                profile: blockzilla_bank_stream_v2::PROFILE.into(),
                evidence_included: false,
                marker,
                objects: envelope
                    .objects
                    .into_iter()
                    .map(|object| blockzilla_bank_stream_v2::Object {
                        path: object.path,
                        offset: object.offset,
                        bytes: object.bytes,
                        sha256: object.sha256,
                    })
                    .collect(),
                event,
            },
            payload,
        )
        .unwrap()
    }

    #[test]
    fn bank_v2_preserves_native_identity_and_execution_outcomes() {
        for version in [0, 1, 2, 255] {
            for failed in [false, true] {
                let frame = bank_fixture(version, failed);
                let bank = decode_bank_payload(&frame).unwrap();
                assert_eq!(
                    serde_json::to_value(&bank).unwrap(),
                    serde_json::to_value(decode_bank_payload_full_projection(&frame).unwrap())
                        .unwrap()
                );
                assert_eq!(bank.bank.bank_id, 5_000_000_000);
                assert_eq!(bank.block_height, 321);
                assert_eq!(bank.transactions.len(), 1);
                assert_eq!(bank.transactions[0].successful_execution, Some(!failed));
                assert_eq!(bank.transactions[0].error.is_some(), failed);
                assert_eq!(bank.transactions[0].signature, base58(&[9; 64]));
                assert!(bank.writable_accounts.contains(&base58(&[1; 32])));
                // Recorded balance changes include readonly loaded addresses.
                if version == 0 || version == 2 {
                    assert!(bank.writable_accounts.contains(&base58(&[8; 32])));
                }
            }
        }
    }

    #[test]
    fn admission_and_full_projection_reject_malformed_metadata() {
        for case in 0..15 {
            let frame = bank_fixture_with_meta(1, false, |meta| match case {
                0 => meta.inner_instructions.as_mut().unwrap()[0].index = 1,
                1 => {
                    meta.inner_instructions.as_mut().unwrap()[0].instructions[0].program_id_index =
                        9
                }
                2 => {
                    meta.inner_instructions.as_mut().unwrap()[0].instructions[0].accounts = vec![9]
                }
                3 => meta.post_token_balances[0].account_index = 9,
                4 => meta.post_token_balances[0].mint = None,
                5 => meta.post_token_balances[0].mint = Some(CompactPubkey::Id(99)),
                6 => meta.post_token_balances[0].owner = Some(CompactPubkey::Id(99)),
                7 => meta.post_token_balances[0].program_id = Some(CompactPubkey::Id(99)),
                8 => meta.rewards[0].reward_type = 255,
                9 => meta.rewards[0].pubkey = CompactPubkey::Id(99),
                10 => meta.return_data.as_mut().unwrap().program_id = CompactPubkey::Id(99),
                11 => meta.pre_balances.clear(),
                12 => meta
                    .logs
                    .as_mut()
                    .unwrap()
                    .events
                    .push(LogEvent::Plain { text: 99 }),
                13 => meta.logs.as_mut().unwrap().events.push(LogEvent::Invoke {
                    program: CompactPubkey::Id(99),
                    depth: 1,
                }),
                14 => meta.rewards[0].reward_type = -1,
                _ => unreachable!(),
            });
            assert!(
                decode_bank_payload_full_projection(&frame).is_err(),
                "full case {case}"
            );
            assert!(
                decode_bank_payload(&frame).is_err(),
                "admission case {case}"
            );
            assert!(
                decode_bank_financial_block(
                    &frame,
                    &DecodeOptions::default(),
                    &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new()),
                )
                .is_err(),
                "filtered case {case}"
            );
        }
    }

    fn rewrite_bank_object(
        frame: &blockzilla_bank_stream_v2::Frame,
        target: &str,
        edit: impl FnOnce(&mut Vec<u8>),
    ) -> blockzilla_bank_stream_v2::Frame {
        let mut edit = Some(edit);
        let metadata_len = u32_at(&frame.wire, 8).unwrap() as usize;
        let body = &frame.wire[16 + metadata_len..];
        let mut objects: Vec<_> = frame
            .envelope
            .objects
            .iter()
            .map(|object| {
                let mut bytes = body[object.offset..object.offset + object.bytes].to_vec();
                if object.path == target {
                    edit.take().unwrap()(&mut bytes);
                }
                (object.path.clone(), bytes)
            })
            .collect();
        assert!(edit.is_none());
        let blob_len = objects
            .iter()
            .find(|(path, _)| path == "archive-v2-blocks.zstd")
            .unwrap()
            .1
            .len();
        let (_, index) = objects
            .iter_mut()
            .find(|(path, _)| path == "archive-v2-blocks.index")
            .unwrap();
        assert_eq!(u32_at(index, 32).unwrap(), 2);
        index[20..28].copy_from_slice(&(blob_len as u64).to_le_bytes());
        index[56..60].copy_from_slice(&(blob_len as u32).to_le_bytes());
        index[60..64].copy_from_slice(&(blob_len as u32).to_le_bytes());
        let mut envelope = frame.envelope.clone();
        let mut payload = Vec::new();
        for (object, (_, bytes)) in envelope.objects.iter_mut().zip(objects) {
            object.offset = payload.len();
            object.bytes = bytes.len();
            object.sha256 = digest(&bytes);
            payload.extend(bytes);
        }
        envelope.marker["files"] = serde_json::to_value(envelope.objects.iter().map(|object|
            json!({"path":object.path,"bytes":object.bytes,"sha256":object.sha256})).collect::<Vec<_>>()).unwrap();
        envelope.event.payload_sha256 = digest(&serde_json::to_vec(&envelope.marker).unwrap());
        blockzilla_bank_stream_v2::encode(envelope, &payload).unwrap()
    }

    fn bank_object<'a>(frame: &'a blockzilla_bank_stream_v2::Frame, name: &str) -> &'a [u8] {
        let metadata_len = u32_at(&frame.wire, 8).unwrap() as usize;
        let object = frame
            .envelope
            .objects
            .iter()
            .find(|object| object.path == name)
            .unwrap();
        let start = 16 + metadata_len + object.offset;
        &frame.wire[start..start + object.bytes]
    }

    fn join_bank_transactions(
        frames: &[blockzilla_bank_stream_v2::Frame],
    ) -> blockzilla_bank_stream_v2::Frame {
        let mut combined: ArchiveV2HotBlockBlob =
            exact(bank_object(&frames[0], "archive-v2-blocks.zstd")).unwrap();
        combined.tx_rows.clear();
        combined.message_bytes.clear();
        combined.metadata_bytes.clear();
        let mut signatures = Vec::new();
        for frame in frames {
            let block: ArchiveV2HotBlockBlob =
                exact(bank_object(frame, "archive-v2-blocks.zstd")).unwrap();
            assert_eq!(block.tx_rows.len(), 1);
            let mut row = block.tx_rows[0];
            row.tx_index = combined.tx_rows.len() as u32;
            row.message_offset = combined.message_bytes.len() as u32;
            row.metadata_offset = combined.metadata_bytes.len() as u32;
            combined.tx_rows.push(row);
            combined.message_bytes.extend(block.message_bytes);
            combined.metadata_bytes.extend(block.metadata_bytes);
            signatures.extend(bank_object(frame, "signatures.bin"));
        }
        combined.tx_count = combined.tx_rows.len() as u32;
        let frame = rewrite_bank_object(&frames[0], "archive-v2-blocks.zstd", |bytes| {
            *bytes = wincode::config::serialize(&combined, wincode_leb128_config()).unwrap();
        });
        let frame = rewrite_bank_object(&frame, "signatures.bin", |bytes| *bytes = signatures);
        rewrite_bank_object(&frame, "archive-v2-blocks.index", |bytes| {
            bytes[64..68].copy_from_slice(&(frames.len() as u32).to_le_bytes());
            bytes[84..88].copy_from_slice(&(frames.len() as u32).to_le_bytes());
        })
    }

    #[test]
    fn financial_filter_selects_all_static_loaded_and_required_keys() {
        for (version, key) in [(255, [1; 32]), (1, [7; 32]), (0, [8; 32]), (2, [8; 32])] {
            let frame = bank_fixture(version, false);
            let full = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
            assert_eq!(full.transaction_indices, vec![0]);
            assert_eq!(full.total_transactions, 1);
            let filtered = decode_bank_financial_block(
                &frame,
                &DecodeOptions::default(),
                &mut FinancialInterest::from_raw(BTreeSet::from([key]), BTreeSet::new()),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&full.block).unwrap(),
                serde_json::to_value(&filtered.block).unwrap(),
                "version {version}"
            );
            let forced = decode_bank_financial_block(
                &frame,
                &required(),
                &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new()),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(full.block).unwrap(),
                serde_json::to_value(forced.block).unwrap()
            );
        }
    }

    #[test]
    fn financial_filter_keeps_original_transaction_positions_and_bank_count() {
        let unrelated = bank_fixture(1, false);
        let selected = rewrite_bank_object(&unrelated, "archive-v2-blocks.zstd", |bytes| {
            let mut block: ArchiveV2HotBlockBlob = exact(bytes).unwrap();
            let mut native: ArchiveV2HotMessagePayload = exact(&block.message_bytes).unwrap();
            let ArchiveV2HotMessagePayload::V1(message) = &mut native else {
                panic!("v1 fixture")
            };
            message.account_keys[0] = CompactPubkey::Raw([42; 32]);
            block.message_bytes =
                wincode::config::serialize(&native, wincode_leb128_config()).unwrap();
            block.tx_rows[0].message_len = block.message_bytes.len() as u32;
            *bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
        });
        let frame = join_bank_transactions(&[unrelated.clone(), selected, unrelated]);
        let full = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
        assert_eq!(full.transaction_indices, vec![0, 1, 2]);
        assert_eq!(full.total_transactions, 3);
        let filtered = decode_bank_financial_block(
            &frame,
            &DecodeOptions::default(),
            &mut FinancialInterest::from_raw(BTreeSet::from([[42; 32]]), BTreeSet::new()),
        )
        .unwrap();
        assert_eq!(filtered.transaction_indices, vec![1]);
        assert_eq!(filtered.total_transactions, 3);
        assert_eq!(filtered.block.transactions.as_ref().unwrap().len(), 1);
        assert_eq!(
            serde_json::to_value(&filtered.block.transactions.as_ref().unwrap()[0]).unwrap(),
            serde_json::to_value(&full.block.transactions.as_ref().unwrap()[1]).unwrap()
        );
    }

    #[test]
    fn parser_fixture_keeps_exact_logs_program_and_original_index() {
        let program = [42; 32];
        let lines = vec![
            format!("Program {} invoke [1]", base58(&program)),
            "Program data: AQID".into(),
            format!("Program {} success", base58(&program)),
        ];
        let frame = bank_fixture_with_program_logs(program, false, Some(lines.clone()), 1, false);
        let filtered = decode_bank_financial_block(
            &frame,
            &DecodeOptions::default(),
            &mut FinancialInterest::from_raw(BTreeSet::from([program]), BTreeSet::new()),
        )
        .unwrap();
        assert_eq!(filtered.transaction_indices, vec![1]);
        assert_eq!(filtered.total_transactions, 2);
        assert!(matches!(
            &filtered.block.transactions.as_ref().unwrap()[0].meta.as_ref().unwrap().log_messages,
            OptionSerializer::Some(actual) if actual == &lines
        ));
        let frame = bank_fixture_with_program_logs(program, false, None, 0, true);
        let decoded = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
        assert!(decoded.block.transactions.unwrap()[0].meta.is_none());
    }

    #[test]
    fn financial_filter_discovers_mint_accounts_within_and_across_banks() {
        let tracked = bank_fixture_with_meta(1, false, |meta| {
            // The mint does not occur in any static or loaded message key.
            meta.post_token_balances[0].mint = Some(CompactPubkey::Raw([42; 32]));
        });
        let activity = bank_fixture_with_meta(1, false, |meta| meta.post_token_balances.clear());
        let unrelated = rewrite_bank_object(&activity, "archive-v2-blocks.zstd", |bytes| {
            let mut block: ArchiveV2HotBlockBlob = exact(bytes).unwrap();
            let mut native: ArchiveV2HotMessagePayload = exact(&block.message_bytes).unwrap();
            let ArchiveV2HotMessagePayload::V1(message) = &mut native else {
                panic!("v1 fixture")
            };
            message.account_keys[0] = CompactPubkey::Raw([22; 32]);
            block.message_bytes =
                wincode::config::serialize(&native, wincode_leb128_config()).unwrap();
            block.tx_rows[0].message_len = block.message_bytes.len() as u32;
            *bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
        });
        let combined = join_bank_transactions(&[tracked.clone(), activity.clone(), unrelated]);
        let full = decode_bank_block(&combined, &DecodeOptions::default()).unwrap();
        let mut interest = FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::from([[42; 32]]));
        let filtered =
            decode_bank_financial_block(&combined, &DecodeOptions::default(), &mut interest)
                .unwrap();
        assert_eq!(filtered.block.transactions.as_ref().unwrap().len(), 2);
        assert_eq!(
            serde_json::to_value(filtered.block.transactions.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&full.block.transactions.as_ref().unwrap()[..2]).unwrap()
        );
        assert!(
            matches!(interest, FinancialInterest::Filtered { ref accounts, .. }
            if accounts.contains(&[1; 32]))
        );

        let mut interest = FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::from([[42; 32]]));
        assert_eq!(
            decode_bank_financial_block(&activity, &DecodeOptions::default(), &mut interest)
                .unwrap()
                .block
                .transactions
                .unwrap()
                .len(),
            0
        );
        decode_bank_financial_block(&tracked, &DecodeOptions::default(), &mut interest).unwrap();
        let later =
            decode_bank_financial_block(&activity, &DecodeOptions::default(), &mut interest)
                .unwrap();
        assert_eq!(later.block.transactions.unwrap().len(), 1);
    }

    #[test]
    fn financial_filter_discovers_loaded_accounts_from_pre_and_post_mints() {
        for pre in [false, true] {
            let frame = bank_fixture_with_meta(0, false, |meta| {
                meta.post_token_balances[0].mint = Some(CompactPubkey::Raw([42; 32]));
                meta.post_token_balances[0].account_index = 2;
                if pre {
                    meta.pre_token_balances = std::mem::take(&mut meta.post_token_balances);
                }
            });
            let mut interest =
                FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::from([[42; 32]]));
            let filtered =
                decode_bank_financial_block(&frame, &DecodeOptions::default(), &mut interest)
                    .unwrap();
            let full = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
            assert_eq!(
                serde_json::to_value(filtered.block).unwrap(),
                serde_json::to_value(full.block).unwrap()
            );
            assert!(
                matches!(interest, FinancialInterest::Filtered { ref accounts, .. }
                if accounts.contains(&[8; 32]))
            );
        }
    }

    #[test]
    fn financial_filter_full_fallback_covers_initial_and_discovered_interest_overflow() {
        let accounts: BTreeSet<[u8; 32]> = (0..MAX_FINANCIAL_INTEREST_KEYS as u64)
            .map(|index| {
                let mut key = [0; 32];
                key[..8].copy_from_slice(&index.to_le_bytes());
                key
            })
            .collect();
        assert!(matches!(
            FinancialInterest::from_raw(accounts.clone(), BTreeSet::from([[42; 32]])),
            FinancialInterest::Full
        ));
        let frame = bank_fixture_with_meta(1, false, |meta| {
            meta.post_token_balances[0].mint = Some(CompactPubkey::Raw([42; 32]));
        });
        let full = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
        let mut interest = FinancialInterest::Filtered {
            accounts: accounts.clone(),
            mints: BTreeSet::from([[42; 32]]),
        };
        let decoded =
            decode_bank_financial_block(&frame, &DecodeOptions::default(), &mut interest).unwrap();
        assert!(matches!(interest, FinancialInterest::Full));
        assert_eq!(
            serde_json::to_value(&decoded.block).unwrap(),
            serde_json::to_value(&full.block).unwrap()
        );
        let mut accounts = accounts;
        accounts.pop_last();
        let mut interest = FinancialInterest::from_raw(accounts, BTreeSet::from([[42; 32]]));
        let decoded =
            decode_bank_financial_block(&frame, &DecodeOptions::default(), &mut interest).unwrap();
        assert!(matches!(interest, FinancialInterest::Full));
        assert_eq!(
            serde_json::to_value(decoded.block).unwrap(),
            serde_json::to_value(full.block).unwrap()
        );
    }

    #[test]
    fn financial_filter_preserves_unknown_metadata_and_missing_alt_failure() {
        for version in [1, 0] {
            let frame = rewrite_bank_object(
                &bank_fixture(version, false),
                "archive-v2-blocks.zstd",
                |bytes| {
                    let mut block: ArchiveV2HotBlockBlob = exact(bytes).unwrap();
                    block.tx_rows[0].flags &=
                        ARCHIVE_V2_TX_FLAG_MESSAGE_V0 | ARCHIVE_V2_TX_FLAG_HAS_LOADED_ADDRESSES;
                    block.tx_rows[0].metadata_len = 0;
                    block.metadata_bytes.clear();
                    *bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
                },
            );
            let full = decode_bank_block(&frame, &DecodeOptions::default());
            let filtered = decode_bank_financial_block(
                &frame,
                &DecodeOptions::default(),
                &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new()),
            );
            if version == 0 {
                assert!(full.is_err());
                assert!(filtered.is_err());
            } else {
                let filtered = filtered.unwrap();
                assert!(filtered.block.transactions.as_ref().unwrap()[0]
                    .meta
                    .is_none());
                assert_eq!(
                    serde_json::to_value(filtered.block).unwrap(),
                    serde_json::to_value(full.unwrap().block).unwrap()
                );
            }
        }
    }

    #[test]
    fn admission_and_full_projection_reject_malformed_message_flags_and_index() {
        let frame = bank_fixture(1, false);
        let bad_index = rewrite_bank_object(&frame, "archive-v2-blocks.index", |bytes| {
            bytes[40..48].copy_from_slice(&101u64.to_le_bytes())
        });
        assert!(decode_bank_payload_full_projection(&bad_index).is_err());
        assert!(decode_bank_payload(&bad_index).is_err());
        assert!(decode_bank_financial_block(
            &bad_index,
            &DecodeOptions::default(),
            &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new())
        )
        .is_err());
        for case in 0..6 {
            let malformed = rewrite_bank_object(&frame, "archive-v2-blocks.zstd", |bytes| {
                let mut block: ArchiveV2HotBlockBlob = exact(bytes).unwrap();
                if case == 5 {
                    block.tx_rows[0].flags ^= ARCHIVE_V2_TX_FLAG_HAS_LOGS;
                } else {
                    let mut native_message: ArchiveV2HotMessagePayload =
                        exact(&block.message_bytes).unwrap();
                    let ArchiveV2HotMessagePayload::V1(message) = &mut native_message else {
                        panic!("v1 fixture")
                    };
                    match case {
                        0 => message.header.num_required_signatures = 2,
                        1 => message.account_keys[0] = CompactPubkey::Id(99),
                        2 => message.recent_blockhash = OwnedCompactRecentBlockhash::Id(99),
                        3 => message.instructions[0].program_id_index = 255,
                        4 => {
                            message.instructions[0].data = ArchiveV2HotInstructionData::System(
                                ArchiveV2SystemInstructionData::AdvanceNonceAccount,
                            )
                        }
                        _ => unreachable!(),
                    }
                    block.message_bytes =
                        wincode::config::serialize(&native_message, wincode_leb128_config())
                            .unwrap();
                    block.tx_rows[0].message_len = block.message_bytes.len() as u32;
                }
                *bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
            });
            assert!(
                decode_bank_payload_full_projection(&malformed).is_err(),
                "full case {case}"
            );
            assert!(
                decode_bank_payload(&malformed).is_err(),
                "admission case {case}"
            );
            assert!(
                decode_bank_financial_block(
                    &malformed,
                    &DecodeOptions::default(),
                    &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new()),
                )
                .is_err(),
                "filtered case {case}"
            );
        }
    }

    #[test]
    fn admission_and_full_projection_agree_on_block_rewards() {
        let frame = bank_fixture(1, false);
        for reward_type in [-1, 0, 1, 2, 3, 4, 255] {
            let with_reward = rewrite_bank_object(&frame, "archive-v2-blocks.zstd", |bytes| {
                let mut block: ArchiveV2HotBlockBlob = exact(bytes).unwrap();
                block.header.rewards = Some(ArchiveV2HotRewards {
                    num_partitions: Some(1),
                    decoded: vec![CompactReward {
                        pubkey: CompactPubkey::Raw([44; 32]),
                        lamports: 1,
                        post_balance: 42,
                        reward_type,
                        commission: None,
                    }],
                });
                *bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
            });
            let full = decode_bank_payload_full_projection(&with_reward);
            let admission = decode_bank_payload(&with_reward);
            let filtered = decode_bank_financial_block(
                &with_reward,
                &DecodeOptions::default(),
                &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new()),
            );
            if (0..=4).contains(&reward_type) {
                assert_eq!(
                    serde_json::to_value(admission.unwrap()).unwrap(),
                    serde_json::to_value(full.unwrap()).unwrap()
                );
                assert!(filtered.is_ok());
            } else {
                assert!(full.is_err());
                assert!(admission.is_err());
                assert!(filtered.is_err());
            }
        }
    }

    #[test]
    fn admission_output_excludes_large_outer_and_inner_instruction_data() {
        let (wire, _, _) = fixture_with_instruction_data(
            1,
            false,
            false,
            Some(1_700_000_000),
            vec![42; 4096],
            |meta| {
                meta.inner_instructions.as_mut().unwrap()[0].instructions[0].data = vec![43; 4096]
            },
        );
        let frame = bank_fixture_from_wire(wire);
        let admission = decode_bank_payload(&frame).unwrap();
        assert_eq!(
            serde_json::to_value(&admission).unwrap(),
            serde_json::to_value(decode_bank_payload_full_projection(&frame).unwrap()).unwrap()
        );
        assert!(serde_json::to_vec(&admission).unwrap().len() < 4096);
        let filtered = decode_bank_financial_block(
            &frame,
            &DecodeOptions::default(),
            &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new()),
        )
        .unwrap();
        assert!(filtered.block.transactions.as_ref().unwrap().is_empty());
        let full = decode_bank_block(&frame, &DecodeOptions::default()).unwrap();
        assert_eq!(filtered.slot, full.slot);
        assert_eq!(filtered.block.blockhash, full.block.blockhash);
        assert_eq!(filtered.block.parent_slot, full.block.parent_slot);
        assert_eq!(filtered.block.block_height, full.block.block_height);
        assert_eq!(filtered.block.block_time, full.block.block_time);
    }

    #[test]
    fn bank_v2_history_preserves_token_coverage_and_exact_receipt_inputs() {
        let frame = bank_fixture(0, false);
        let decoded = decode_bank_block(&frame, &required()).unwrap();
        assert_eq!(decoded.slot, 100);
        assert_eq!(decoded.block.block_time, Some(1_700_000_000));
        assert_eq!(decoded.block.block_height, Some(321));
        let tx = &decoded.block.transactions.unwrap()[0];
        let meta = tx.meta.as_ref().unwrap();
        assert!(matches!(meta.pre_token_balances, OptionSerializer::Skip));
        assert!(
            matches!(&meta.post_token_balances, OptionSerializer::Some(values) if values.len() == 1)
        );
        assert!(
            matches!(&meta.loaded_addresses, OptionSerializer::Some(values) if values.readonly.len() == 1)
        );
        assert!(
            matches!(&meta.log_messages, OptionSerializer::Some(values) if values.iter().any(|line| line == "Program data: AQID"))
        );

        let ambiguous = bank_fixture_with_meta(1, false, |meta| meta.post_token_balances.clear());
        let decoded = decode_bank_block(&ambiguous, &required()).unwrap();
        let meta = decoded
            .block
            .transactions
            .unwrap()
            .pop()
            .unwrap()
            .meta
            .unwrap();
        assert!(matches!(meta.pre_token_balances, OptionSerializer::Skip));
        assert!(matches!(meta.post_token_balances, OptionSerializer::Skip));
    }

    #[test]
    fn bank_v2_history_rejects_missing_or_truncated_required_receipts() {
        let missing = bank_fixture_with_meta(1, false, |meta| meta.logs = None);
        assert!(decode_bank_block(&missing, &required()).is_err());
        assert!(decode_bank_financial_block(
            &missing,
            &required(),
            &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new())
        )
        .is_err());
        let truncated = bank_fixture_with_meta(1, false, |meta| {
            meta.logs
                .as_mut()
                .unwrap()
                .events
                .push(LogEvent::LogTruncated);
        });
        assert!(decode_bank_block(&truncated, &required()).is_err());
        assert!(decode_bank_financial_block(
            &truncated,
            &required(),
            &mut FinancialInterest::from_raw(BTreeSet::new(), BTreeSet::new())
        )
        .is_err());
        // Failed execution has no successful financial receipt to account for.
        let failed = bank_fixture_with_meta(1, true, |meta| meta.logs = None);
        assert!(decode_bank_block(&failed, &required()).is_ok());
    }

    #[test]
    fn bank_v2_rejects_native_height_mismatch_and_forged_frame_fields() {
        let frame = bank_fixture(1, false);
        let mut forged = frame.clone();
        forged.sha256 = "0".repeat(64);
        assert!(decode_bank_payload(&forged).is_err());
        let metadata_len = u32_at(&frame.wire, 8).unwrap() as usize;
        let mut envelope = frame.envelope.clone();
        envelope.event.block_height = Some(322);
        let wrong =
            blockzilla_bank_stream_v2::encode(envelope, &frame.wire[16 + metadata_len..]).unwrap();
        assert!(decode_bank_payload(&wrong)
            .unwrap_err()
            .to_string()
            .contains("height"));
    }

    #[test]
    #[ignore = "requires an operator-verified retained frame in FERNO_BLOCKZILLA_SAMPLE_FRAME"]
    fn bank_v2_decodes_retained_operator_sample() {
        let path = std::env::var("FERNO_BLOCKZILLA_SAMPLE_FRAME").unwrap();
        let wire = std::fs::read(path).unwrap();
        let expected = std::env::var("FERNO_BLOCKZILLA_SAMPLE_SHA256").unwrap();
        assert_eq!(digest(&wire), expected);
        let frame = blockzilla_bank_stream_v2::decode(wire.into()).unwrap();
        let bank = decode_bank_payload(&frame).unwrap();
        assert_eq!(
            serde_json::to_value(&bank).unwrap(),
            serde_json::to_value(decode_bank_payload_full_projection(&frame).unwrap()).unwrap()
        );
        assert_eq!(bank.bank, frame.envelope.event.bank);
        assert_eq!(Some(bank.block_height), frame.envelope.event.block_height);
        assert!(!bank.transactions.is_empty());
        assert!(bank
            .transactions
            .iter()
            .all(|tx| tx.successful_execution.is_some()));
        eprintln!(
            "retained sample: {} transactions, {} failed, {} unknown",
            bank.transactions.len(),
            bank.transactions
                .iter()
                .filter(|tx| tx.successful_execution == Some(false))
                .count(),
            bank.transactions
                .iter()
                .filter(|tx| tx.successful_execution.is_none())
                .count()
        );
    }

    #[test]
    fn bank_v2_missing_metadata_is_unknown_execution() {
        let frame = bank_fixture(1, false);
        let metadata_len = u32_at(&frame.wire, 8).unwrap() as usize;
        let body = &frame.wire[16 + metadata_len..];
        let mut envelope = frame.envelope.clone();
        let mut replacement: Vec<blockzilla_bank_stream_v2::Object> = Vec::new();
        let mut objects = Vec::new();
        let mut files = Vec::new();
        let mut raw = Vec::new();
        for object in &envelope.objects {
            let mut bytes = body[object.offset..object.offset + object.bytes].to_vec();
            if object.path == "archive-v2-blocks.zstd" {
                let mut block: ArchiveV2HotBlockBlob = exact(&bytes).unwrap();
                block.tx_rows[0].flags = 0;
                block.tx_rows[0].metadata_len = 0;
                block.metadata_bytes.clear();
                bytes = wincode::config::serialize(&block, wincode_leb128_config()).unwrap();
                raw = bytes.clone();
            }
            // The block object follows its index, so rebuild the index below.
            objects.push((object.path.clone(), bytes));
        }
        for (path, mut bytes) in objects {
            if path == "archive-v2-blocks.index" {
                bytes[20..28].copy_from_slice(&(raw.len() as u64).to_le_bytes());
                bytes[56..60].copy_from_slice(&(raw.len() as u32).to_le_bytes());
                bytes[60..64].copy_from_slice(&(raw.len() as u32).to_le_bytes());
            }
            let hash = digest(&bytes);
            files.push(json!({"path":path,"bytes":bytes.len(),"sha256":hash}));
            replacement.push(blockzilla_bank_stream_v2::Object {
                path,
                offset: replacement.iter().map(|object| object.bytes).sum(),
                bytes: bytes.len(),
                sha256: hash,
            });
        }
        let mut payload = Vec::new();
        for object in &replacement {
            let original = envelope
                .objects
                .iter()
                .find(|old| old.path == object.path)
                .unwrap();
            let mut bytes = body[original.offset..original.offset + original.bytes].to_vec();
            if object.path == "archive-v2-blocks.zstd" {
                bytes = raw.clone();
            }
            if object.path == "archive-v2-blocks.index" {
                bytes[20..28].copy_from_slice(&(raw.len() as u64).to_le_bytes());
                bytes[56..60].copy_from_slice(&(raw.len() as u32).to_le_bytes());
                bytes[60..64].copy_from_slice(&(raw.len() as u32).to_le_bytes());
            }
            payload.extend(bytes);
        }
        envelope.objects = replacement;
        envelope.marker["files"] = files.into();
        let missing = blockzilla_bank_stream_v2::encode(envelope, &payload).unwrap();
        let bank = decode_bank_payload(&missing).unwrap();
        assert_eq!(bank.transactions[0].successful_execution, None);
        assert_eq!(bank.transactions[0].error, None);
    }

    #[test]
    fn decodes_compressed_v0_exact_balances_cpi_logs_and_time() {
        let (wire, notice, after) = fixture(0, false, true, Some(1_700_000_000), |_| {});
        let decoded = decode_chunk(&wire, &notice, &after, None, &required()).unwrap();
        assert_eq!(decoded.checkpoint.last_slot, 100);
        assert_eq!(decoded.blocks[0].block.block_time, Some(1_700_000_000));
        assert_eq!(decoded.blocks[0].block.block_height, Some(321));
        let tx = &decoded.blocks[0].block.transactions.as_ref().unwrap()[0];
        let meta = tx.meta.as_ref().unwrap();
        assert_eq!(meta.pre_balances[0], u64::MAX);
        match &meta.post_token_balances {
            OptionSerializer::Some(balances) => {
                assert_eq!(balances[0].ui_token_amount.amount, u64::MAX.to_string());
                assert_eq!(
                    balances[0].ui_token_amount.ui_amount_string,
                    "18446744073.709551615"
                );
            }
            _ => panic!("token balances missing"),
        }
        match &meta.loaded_addresses {
            OptionSerializer::Some(loaded) => assert_eq!(loaded.readonly, vec![base58(&[8; 32])]),
            _ => panic!("loaded missing"),
        }
        match &meta.inner_instructions {
            OptionSerializer::Some(inner) => match &inner[0].instructions[0] {
                UiInstruction::Compiled(ix) => {
                    assert_eq!(ix.program_id_index, 2);
                    assert_eq!(bs58::decode(&ix.data).into_vec().unwrap(), [9, 8]);
                }
                _ => panic!("CPI missing"),
            },
            _ => panic!("inner missing"),
        }
        match &meta.log_messages {
            OptionSerializer::Some(logs) => assert_eq!(
                logs,
                &vec![
                    format!("Program {} invoke [1]", base58(&[7; 32])),
                    "Program data: AQID".into(),
                    format!("Program {} success", base58(&[7; 32]))
                ]
            ),
            _ => panic!("logs missing"),
        }
        assert!(meta.err.is_none() && meta.status.is_ok());
        assert_eq!(
            serde_json::to_value(&meta.rewards).unwrap()[0]["rewardType"],
            "Fee"
        );
    }
    #[test]
    fn supports_legacy_v1_and_failed_status() {
        for version in [1, 2] {
            let (wire, notice, after) = fixture(version, true, false, Some(1_700_000_000), |_| {});
            let decoded = decode_chunk(&wire, &notice, &after, None, &required()).unwrap();
            let tx = &decoded.blocks[0].block.transactions.as_ref().unwrap()[0];
            assert!(
                tx.meta.as_ref().unwrap().err.is_some()
                    && tx.meta.as_ref().unwrap().status.is_err()
            );
            match &tx.transaction {
                EncodedTransaction::Json(tx) => match &tx.message {
                    UiMessage::Raw(message) => assert_eq!(
                        message.transaction_config.as_ref().map(|c| c.priority_fee),
                        if version == 1 {
                            Some(Some(u64::MAX))
                        } else {
                            None
                        }
                    ),
                    _ => panic!("raw missing"),
                },
                _ => panic!("json missing"),
            }
        }
    }
    #[test]
    fn rejects_corruption_gap_source_change_and_missing_receipt_coverage() {
        let (wire, notice, after) = fixture(0, false, false, Some(1_700_000_000), |_| {});
        let mut corrupt = wire.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_chunk(&corrupt, &notice, &after, None, &required()).is_err());
        assert!(decode_chunk(&wire, &notice, "test:98:-", None, &required()).is_err());
        assert!(decode_chunk(&wire, &notice, "foreign:99:-", None, &required()).is_err());
        let previous = ChunkCheckpoint {
            cursor: after.clone(),
            session: "test".into(),
            last_slot: 99,
            last_blockhash: base58(&[3; 32]),
            source: SourceIdentity {
                genesis_hash: "wrong".into(),
                node_identity: "node".into(),
                agave_revision: "revision".into(),
            },
        };
        assert!(decode_chunk(&wire, &notice, &after, Some(&previous), &required()).is_err());
        for edit in [
            |meta: &mut CompactMetaV1| meta.logs = None,
            |meta: &mut CompactMetaV1| meta.inner_instructions = None,
            |meta: &mut CompactMetaV1| {
                meta.logs
                    .as_mut()
                    .unwrap()
                    .events
                    .push(LogEvent::LogTruncated)
            },
        ] {
            let (wire, notice, after) = fixture(0, false, false, Some(1_700_000_000), edit);
            assert!(decode_chunk(&wire, &notice, &after, None, &required()).is_err());
        }
        let (wire, notice, after) = fixture(0, false, false, None, |_| {});
        assert!(decode_chunk(&wire, &notice, &after, None, &required()).is_err());
    }
    #[test]
    fn rejects_invalid_log_refs_and_future_time_without_panicking() {
        let (wire, notice, after) = fixture(0, false, false, Some(1_700_000_000), |meta| {
            meta.logs
                .as_mut()
                .unwrap()
                .events
                .push(LogEvent::Plain { text: 99 });
        });
        assert!(decode_chunk(&wire, &notice, &after, None, &required()).is_err());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (wire, notice, after) = fixture(0, false, false, Some(now as i64 + 60), |_| {});
        assert!(decode_chunk(&wire, &notice, &after, None, &required()).is_err());
    }
    #[test]
    fn reconstructs_system_and_compute_budget_bytes() {
        assert_eq!(
            instruction_bytes(&ArchiveV2HotInstructionData::System(
                ArchiveV2SystemInstructionData::Transfer { lamports: 7 }
            ))
            .unwrap()
            .unwrap(),
            [2u32.to_le_bytes().to_vec(), 7u64.to_le_bytes().to_vec()].concat()
        );
        assert_eq!(
            instruction_bytes(&ArchiveV2HotInstructionData::ComputeBudget(
                ArchiveV2ComputeBudgetInstructionData::SetComputeUnitPrice(u64::MAX)
            ))
            .unwrap()
            .unwrap(),
            [vec![3], u64::MAX.to_le_bytes().to_vec()].concat()
        );
    }

    #[test]
    fn keeps_compact_vote_semantics_and_outer_instruction_positions() {
        let vote: [u8; 32] = bs58::decode("Vote111111111111111111111111111111111111111")
            .into_vec()
            .unwrap()
            .try_into()
            .unwrap();
        let raw = || ArchiveV2HotInstruction {
            program_id_index: 2,
            accounts: vec![0],
            data: ArchiveV2HotInstructionData::Raw(vec![33, 7, 0]),
        };
        let message = ArchiveV2HotMessagePayload::Legacy(ArchiveV2HotLegacyMessage {
            header: CompactMessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 2,
            },
            account_keys: vec![
                CompactPubkey::Raw([1; 32]),
                CompactPubkey::Raw(vote),
                CompactPubkey::Raw([7; 32]),
            ],
            recent_blockhash: OwnedCompactRecentBlockhash::Nonce([3; 32]),
            instructions: vec![
                raw(),
                ArchiveV2HotInstruction {
                    program_id_index: 1,
                    accounts: vec![0],
                    data: ArchiveV2HotInstructionData::VoteTowerSync(ArchiveV2VoteTowerSync {
                        update: ArchiveV2VoteStateUpdate {
                            root: None,
                            lockout_offsets: vec![],
                            hash: ArchiveV2VoteHashRef::Zero,
                            timestamp: None,
                        },
                        block_id_hash: ArchiveV2VoteHashRef::Zero,
                    }),
                },
                raw(),
            ],
        });
        let tx = convert_transaction(
            message,
            None,
            ARCHIVE_V2_TX_FLAG_HAS_COMPACT_VOTE_IX,
            1,
            vec![base58(&[9; 64])],
            &[],
            &[],
            &[3; 40],
            &BTreeSet::new(),
        )
        .unwrap();
        match tx.transaction {
            EncodedTransaction::Json(tx) => match tx.message {
                UiMessage::Parsed(message) => {
                    assert_eq!(message.account_keys.len(), 3);
                    assert!(message.account_keys[0].signer && message.account_keys[0].writable);
                    assert!(!message.account_keys[1].signer && !message.account_keys[1].writable);
                    assert_eq!(message.instructions.len(), 3);
                    for index in [0, 2] {
                        match &message.instructions[index] {
                            UiInstruction::Compiled(ix) => {
                                assert_eq!(bs58::decode(&ix.data).into_vec().unwrap(), [33, 7, 0])
                            }
                            _ => panic!("raw data was not kept"),
                        }
                    }
                    match &message.instructions[1] {
                        UiInstruction::Parsed(UiParsedInstruction::Parsed(ix)) => {
                            assert_eq!(ix.program_id, base58(&vote));
                            assert!(ix.parsed["blockzillaCompactInstruction"]["VoteTowerSync"]
                                .is_object());
                        }
                        _ => panic!("compact vote semantic value missing"),
                    }
                }
                _ => panic!("mixed message missing"),
            },
            _ => panic!("transaction missing"),
        }
    }
}
