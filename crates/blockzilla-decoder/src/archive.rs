//! Checked binary archive reads. The public `.bin` envelope is not a BZSTRM01 frame.
//!
//! The bundle does not carry a genesis hash or archive generation. The caller
//! must admit its origin, generation, and coverage before calling this decoder.
use super::*;
use std::collections::BTreeMap;

pub const MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ACCESS_ROWS: usize = 262_144;

#[derive(Clone, Debug)]
pub struct ExpectedArchiveBlock {
    pub slot: u64,
    pub blockhash: String,
    pub parent_slot: Option<u64>,
    pub previous_blockhash: Option<String>,
    pub block_height: Option<u64>,
}

struct AccessKeys(BTreeMap<u32, [u8; 32]>);
impl KeyLookup for AccessKeys {
    fn lookup_key(&self, id: u32) -> Option<[u8; 32]> {
        self.0.get(&id).copied()
    }
}

/// Decode the full block-local archive bundle without an RPC JSON intermediate.
/// Finality and archive coverage admission remain the caller's responsibility.
pub fn decode_bundle(
    wire: &[u8],
    expected: &ExpectedArchiveBlock,
    options: &DecodeOptions,
) -> Result<DecodedBlock> {
    ensure!(
        !wire.is_empty() && wire.len() <= MAX_BUNDLE_BYTES,
        "archive bundle exceeds byte limit"
    );
    let bundle: BlockzillaGetBlockBundleV1 = wincode::config::deserialize_exact(
        wire,
        bounded_historical_source_wincode_leb128_config::<MAX_BUNDLE_BYTES>(),
    )
    .context("invalid binary archive bundle")?;
    ensure!(
        bundle.version == WINCODE_BLOCKZILLA_GET_BLOCK_BUNDLE_VERSION,
        "unsupported archive bundle version"
    );
    ensure!(
        bundle.slot == expected.slot && expected.slot <= SAFE_INTEGER,
        "archive bundle slot differs from requested block"
    );
    let hot = unpack(bundle.hot_block_encoding, &bundle.hot_block, MAX_BLOCK)?;
    let block = deserialize_archive_v2_hot_block_blob_borrowed_current_with_preallocation_limit::<
        1_048_576,
    >(&hot)
    .context("invalid current-schema archive hot block")?;
    ensure!(
        block.header.slot == expected.slot,
        "archive hot block slot differs from requested block"
    );
    ensure!(
        block.tx_count as usize <= MAX_TX && block.tx_rows_len() == block.tx_count as usize,
        "archive transaction count exceeds limit or differs from rows"
    );
    ensure!(
        (expected.slot == 0 && block.header.parent_slot == 0)
            || block.header.parent_slot < expected.slot,
        "invalid archive parent slot"
    );
    let access_wire = unpack(
        bundle.block_access_encoding,
        bundle
            .block_access
            .as_deref()
            .context("binary archive bundle requires full block access data")?,
        MAX_BUNDLE_BYTES,
    )?;
    let access = decode_access(&access_wire)?;
    let expected_hash = decode_hash(&expected.blockhash)?;
    ensure!(
        access.blockhash == expected_hash,
        "archive block hash differs from requested block"
    );
    if let Some(parent) = expected.parent_slot {
        ensure!(
            block.header.parent_slot == parent,
            "archive parent slot differs from requested block"
        );
    }
    if let Some(hash) = &expected.previous_blockhash {
        ensure!(
            access.previous_blockhash == decode_hash(hash)?,
            "archive parent hash differs from requested block"
        );
    }
    let height = block
        .header
        .block_height
        .context("archive block height is unavailable")?;
    ensure!(height <= SAFE_INTEGER, "archive block height exceeds limit");
    if let Some(expected_height) = expected.block_height {
        ensure!(
            height == expected_height,
            "archive block height differs from requested block"
        );
    }
    ensure!(access.flags == 0, "unsupported archive access flags");
    ensure!(
        access.signature_counts.len() == block.tx_count as usize,
        "archive signature count rows differ from transactions"
    );
    ensure!(
        access.pubkeys.len() <= MAX_ACCESS_ROWS
            && access.blockhashes.len() <= MAX_ACCESS_ROWS
            && access.vote_hashes.len() <= MAX_ACCESS_ROWS,
        "archive access dictionaries exceed limit"
    );
    let mut keys = AccessKeys(BTreeMap::new());
    for entry in &access.pubkeys {
        ensure!(
            entry.id != 0 && keys.0.insert(entry.id, entry.pubkey).is_none(),
            "duplicate or zero archive pubkey ID"
        );
    }
    let mut hashes = BTreeMap::new();
    for entry in &access.blockhashes {
        ensure!(
            hashes.insert(entry.id, entry.blockhash).is_none(),
            "duplicate archive blockhash ID"
        );
    }
    let mut vote_hashes = BTreeMap::new();
    for entry in &access.vote_hashes {
        ensure!(
            vote_hashes
                .insert(entry.block_id, (entry.bank_hash, entry.block_id_hash))
                .is_none(),
            "duplicate archive vote hash ID"
        );
    }
    let total_signatures = access
        .signature_counts
        .iter()
        .try_fold(0usize, |sum, count| {
            sum.checked_add(*count as usize)
                .context("archive signature count overflow")
        })?;
    ensure!(
        total_signatures.checked_mul(64) == Some(access.signatures.len()),
        "archive signature bytes differ from counts"
    );
    let required: BTreeSet<_> = options
        .required_programs
        .iter()
        .map(String::as_str)
        .collect();
    for program in &required {
        decode_hash(program).context("invalid required program key")?;
    }
    let tail = [
        access.previous_blockhash.as_slice(),
        &block.header.parent_slot.to_le_bytes(),
    ]
    .concat();
    let mut transactions = Vec::with_capacity(block.tx_count as usize);
    let mut transaction_indices = Vec::with_capacity(block.tx_count as usize);
    let mut output_budget = OutputBudget(0);
    let (mut message_at, mut metadata_at, mut signature_at) = (0usize, 0usize, 0usize);
    for (index, row) in block.tx_rows().enumerate() {
        ensure!(
            row.tx_index as usize == index && row.reserved == [0; 3] && row.flags & !2047 == 0,
            "invalid archive transaction row"
        );
        ensure!(
            row.flags
                & (ARCHIVE_V2_TX_FLAG_TX_RAW_FALLBACK | ARCHIVE_V2_TX_FLAG_METADATA_RAW_FALLBACK)
                == 0,
            "raw archive fallback is unsupported"
        );
        ensure!(
            row.message_offset as usize == message_at
                && row.metadata_offset as usize == metadata_at,
            "noncontiguous archive transaction regions"
        );
        message_at = message_at
            .checked_add(row.message_len as usize)
            .context("archive message overflow")?;
        metadata_at = metadata_at
            .checked_add(row.metadata_len as usize)
            .context("archive metadata overflow")?;
        let mut message: ArchiveV2HotMessagePayload = exact(
            block
                .message_bytes
                .get(row.message_offset as usize..message_at)
                .context("archive message outside block")?,
        )?;
        normalize_hashes(&mut message, &hashes, &vote_hashes)?;
        let metadata: Option<CompactMetaV1> = if row.flags & ARCHIVE_V2_TX_FLAG_HAS_METADATA != 0 {
            Some(exact(
                block
                    .metadata_bytes
                    .get(row.metadata_offset as usize..metadata_at)
                    .context("archive metadata outside block")?,
            )?)
        } else {
            ensure!(
                row.metadata_len == 0 && row.flags & ARCHIVE_V2_TX_FLAG_HAS_ERROR == 0,
                "archive metadata is missing"
            );
            None
        };
        ensure!(
            access.signature_counts[index] == row.signature_count,
            "archive signature count differs from transaction row"
        );
        let signature_end = signature_at
            .checked_add(row.signature_count as usize * 64)
            .context("archive signature offset overflow")?;
        let signatures = access
            .signatures
            .get(signature_at..signature_end)
            .context("archive signatures outside access data")?;
        signature_at = signature_end;
        let transaction = convert_transaction(
            message,
            metadata,
            row.flags,
            row.signature_count,
            signatures
                .as_chunks::<64>()
                .0
                .iter()
                .map(|value| base58(value))
                .collect(),
            &keys,
            &[],
            &tail,
            &required,
        )?;
        serde_json::to_writer(&mut output_budget, &transaction)
            .context("decoded archive transaction output exceeds limit")?;
        transactions.push(transaction);
        transaction_indices.push(row.tx_index);
    }
    ensure!(
        message_at == block.message_bytes.len()
            && metadata_at == block.metadata_bytes.len()
            && signature_at == access.signatures.len(),
        "unconsumed archive transaction region"
    );
    let block_time = block.header.block_time;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    ensure!(
        block_time.is_none_or(|time| time >= 0 && time as u64 <= now.saturating_add(5)),
        "archive block time is invalid or in the future"
    );
    if transactions
        .iter()
        .any(|tx| transaction_requires_receipts(tx, &required))
    {
        ensure!(
            block_time.is_some(),
            "required archive receipt block time is unavailable"
        );
    }
    let rewards = block
        .header
        .rewards
        .as_ref()
        .map(|rewards| {
            rewards
                .decoded
                .iter()
                .map(|reward| convert_reward(reward, &keys))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let mut decoded = DecodedBlock {
        slot: expected.slot,
        total_transactions: block.tx_count as usize,
        transaction_indices,
        block: UiConfirmedBlock {
            previous_blockhash: base58(&access.previous_blockhash),
            blockhash: base58(&access.blockhash),
            parent_slot: block.header.parent_slot,
            transactions: Some(transactions),
            signatures: None,
            rewards,
            num_reward_partitions: block.header.rewards.as_ref().and_then(|r| r.num_partitions),
            block_time,
            block_height: Some(height),
        },
    };
    // Compact metadata does not distinguish an omitted token list from an
    // explicitly empty list. Keep the same conservative evidence as live input.
    normalize_bank_token_coverage(&mut decoded);
    Ok(decoded)
}

fn decode_hash(text: &str) -> Result<[u8; 32]> {
    ensure!(
        (32..=44).contains(&text.len()),
        "invalid archive hash length"
    );
    let bytes = bs58::decode(text)
        .into_vec()
        .context("invalid archive hash")?;
    let hash: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("archive hash must have 32 bytes"))?;
    ensure!(base58(&hash) == text, "noncanonical archive hash");
    Ok(hash)
}

fn unpack(encoding: BlockzillaGetBlockBlobEncoding, input: &[u8], limit: usize) -> Result<Vec<u8>> {
    ensure!(
        !input.is_empty() && input.len() <= MAX_BUNDLE_BYTES,
        "archive blob exceeds encoded byte limit"
    );
    match encoding {
        BlockzillaGetBlockBlobEncoding::Wincode => {
            ensure!(
                input.len() <= limit,
                "archive blob exceeds decoded byte limit"
            );
            Ok(input.to_vec())
        }
        BlockzillaGetBlockBlobEncoding::ZstdWincode => {
            let mut decoder = StreamingDecoder::new_with_max_window_size(input, limit as u64)
                .context("invalid archive zstd header or window size")?;
            let mut bytes = Vec::with_capacity(input.len().min(limit));
            (&mut decoder)
                .take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .context("invalid archive zstd frame")?;
            ensure!(
                bytes.len() <= limit,
                "archive blob exceeds decoded byte limit"
            );
            ensure!(
                decoder.into_inner().is_empty(),
                "trailing archive zstd bytes"
            );
            Ok(bytes)
        }
    }
}

#[derive(wincode::SchemaRead)]
struct LegacyAccessV1 {
    version: u16,
    flags: u32,
    blockhash: [u8; 32],
    previous_blockhash: [u8; 32],
    signature_counts: Vec<u8>,
    signatures: Vec<u8>,
    pubkeys: Vec<ArchiveV2BlockAccessPubkey>,
    blockhashes: Vec<ArchiveV2BlockAccessBlockhash>,
}

fn decode_access(bytes: &[u8]) -> Result<ArchiveV2BlockAccessBlob> {
    let config = bounded_historical_source_wincode_leb128_config::<MAX_BUNDLE_BYTES>();
    let version: u16 = wincode::config::deserialize(bytes, config)?;
    match version {
        WINCODE_ARCHIVE_V2_BLOCK_ACCESS_VERSION => {
            Ok(wincode::config::deserialize_exact(bytes, config)?)
        }
        1 => {
            // The explicit V1 grammar comes from Blockzilla's Worker contract.
            // Never retry a failed current record with a different grammar.
            let legacy: LegacyAccessV1 = wincode::config::deserialize_exact(bytes, config)?;
            Ok(ArchiveV2BlockAccessBlob {
                version: legacy.version,
                flags: legacy.flags,
                blockhash: legacy.blockhash,
                previous_blockhash: legacy.previous_blockhash,
                signature_counts: legacy.signature_counts,
                signatures: legacy.signatures,
                pubkeys: legacy.pubkeys,
                blockhashes: legacy.blockhashes,
                vote_hashes: Vec::new(),
            })
        }
        _ => anyhow::bail!("unsupported archive block access version"),
    }
}

type VoteHashes = BTreeMap<u32, (Option<[u8; 32]>, Option<[u8; 32]>)>;
fn normalize_hashes(
    message: &mut ArchiveV2HotMessagePayload,
    hashes: &BTreeMap<i32, [u8; 32]>,
    votes: &VoteHashes,
) -> Result<()> {
    let (recent, instructions) = match message {
        ArchiveV2HotMessagePayload::Legacy(m) => (&mut m.recent_blockhash, &mut m.instructions),
        ArchiveV2HotMessagePayload::V0(m) => (&mut m.recent_blockhash, &mut m.instructions),
        ArchiveV2HotMessagePayload::V1(m) => (&mut m.recent_blockhash, &mut m.instructions),
    };
    if let OwnedCompactRecentBlockhash::Id(id) = recent {
        *recent = OwnedCompactRecentBlockhash::Nonce(
            *hashes
                .get(id)
                .context("archive access is missing a recent blockhash")?,
        );
    }
    let resolve = |value: &mut ArchiveV2VoteHashRef, column: u8| -> Result<()> {
        if let ArchiveV2VoteHashRef::Block(id) = value {
            ensure!(
                column < 2,
                "auxiliary vote hash cannot reference a block ID"
            );
            let pair = votes
                .get(id)
                .context("archive access is missing a vote hash")?;
            let hash = if column == 0 { pair.0 } else { pair.1 }
                .context("archive access is missing the required vote hash column")?;
            *value = ArchiveV2VoteHashRef::Raw(hash);
        }
        Ok(())
    };
    for instruction in instructions {
        match &mut instruction.data {
            ArchiveV2HotInstructionData::VoteCompactUpdateVoteState(update) => {
                resolve(&mut update.hash, 0)?
            }
            ArchiveV2HotInstructionData::VoteCompactUpdateVoteStateSwitch {
                update,
                switch_proof_hash,
            } => {
                resolve(&mut update.hash, 0)?;
                resolve(switch_proof_hash, 2)?;
            }
            ArchiveV2HotInstructionData::VoteTowerSync(tower) => {
                resolve(&mut tower.update.hash, 0)?;
                resolve(&mut tower.block_id_hash, 1)?;
            }
            ArchiveV2HotInstructionData::VoteTowerSyncSwitch {
                tower,
                switch_proof_hash,
            } => {
                resolve(&mut tower.update.hash, 0)?;
                resolve(&mut tower.block_id_hash, 1)?;
                resolve(switch_proof_hash, 2)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
pub mod tests {
    use super::*;
    use blockzilla_primitives::wincode_leb128_config;

    pub fn fixture(version: u8, failed: bool, compressed: bool) -> Vec<u8> {
        let (wire, _, _) =
            crate::tests::fixture(version, failed, compressed, Some(1_700_000_000), |_| {});
        let n = u32_at(&wire, 8).unwrap() as usize;
        let envelope: Envelope = serde_json::from_slice(&wire[16..16 + n]).unwrap();
        let payload = &wire[16 + n..];
        let object = |name: &str| {
            let row = envelope.objects.iter().find(|o| o.path == name).unwrap();
            payload[row.offset..row.offset + row.bytes].to_vec()
        };
        let access = ArchiveV2BlockAccessBlob {
            version: WINCODE_ARCHIVE_V2_BLOCK_ACCESS_VERSION,
            flags: 0,
            blockhash: [2; 32],
            previous_blockhash: [3; 32],
            signature_counts: vec![1],
            signatures: vec![9; 64],
            pubkeys: vec![
                ArchiveV2BlockAccessPubkey {
                    id: 1,
                    pubkey: [7; 32],
                },
                ArchiveV2BlockAccessPubkey {
                    id: 2,
                    pubkey: [8; 32],
                },
            ],
            blockhashes: vec![],
            vote_hashes: vec![],
        };
        encode(&BlockzillaGetBlockBundleV1 {
            version: WINCODE_BLOCKZILLA_GET_BLOCK_BUNDLE_VERSION,
            slot: 100,
            hot_block_encoding: if compressed {
                BlockzillaGetBlockBlobEncoding::ZstdWincode
            } else {
                BlockzillaGetBlockBlobEncoding::Wincode
            },
            hot_block: object("archive-v2-blocks.zstd"),
            block_access_encoding: BlockzillaGetBlockBlobEncoding::Wincode,
            block_access: Some(
                wincode::config::serialize(&access, wincode_leb128_config()).unwrap(),
            ),
        })
    }
    fn encode(bundle: &BlockzillaGetBlockBundleV1) -> Vec<u8> {
        wincode::config::serialize(bundle, wincode_leb128_config()).unwrap()
    }
    pub fn expectation() -> ExpectedArchiveBlock {
        ExpectedArchiveBlock {
            slot: 100,
            blockhash: base58(&[2; 32]),
            parent_slot: Some(99),
            previous_blockhash: Some(base58(&[3; 32])),
            block_height: Some(321),
        }
    }
    fn edit(bytes: &[u8], change: impl FnOnce(&mut BlockzillaGetBlockBundleV1)) -> Vec<u8> {
        let mut bundle: BlockzillaGetBlockBundleV1 =
            wincode::config::deserialize_exact(bytes, wincode_leb128_config()).unwrap();
        change(&mut bundle);
        encode(&bundle)
    }
    #[test]
    fn binary_bundle_preserves_versions_errors_metadata_and_parent() {
        for version in [0, 1, 9] {
            for failed in [false, true] {
                for compressed in [false, true] {
                    let decoded = decode_bundle(
                        &fixture(version, failed, compressed),
                        &expectation(),
                        &DecodeOptions::default(),
                    )
                    .unwrap();
                    assert_eq!(decoded.block.parent_slot, 99);
                    assert_eq!(decoded.block.block_height, Some(321));
                    let tx = &decoded.block.transactions.unwrap()[0];
                    assert!(matches!(
                        tx.meta.as_ref().unwrap().pre_token_balances,
                        OptionSerializer::Skip
                    ));
                    assert_eq!(tx.meta.as_ref().unwrap().err.is_some(), failed);
                    assert_eq!(tx.meta.as_ref().unwrap().fee, 5000);
                    assert_eq!(
                        tx.version,
                        Some(match version {
                            0 => TransactionVersion::Number(0),
                            1 => TransactionVersion::Number(1),
                            _ => TransactionVersion::LEGACY,
                        })
                    );
                    let EncodedTransaction::Json(tx) = &tx.transaction else {
                        panic!("typed transaction projection required")
                    };
                    assert_eq!(tx.signatures[0], base58(&[9; 64]));
                }
            }
        }
    }
    #[test]
    fn identity_mismatch_and_missing_access_fail_closed() {
        let bytes = fixture(0, false, true);
        for change in [0, 1, 2, 3, 4] {
            let mut expected = expectation();
            match change {
                0 => expected.slot += 1,
                1 => expected.blockhash = base58(&[4; 32]),
                2 => expected.parent_slot = Some(98),
                3 => expected.previous_blockhash = Some(base58(&[4; 32])),
                _ => expected.block_height = Some(322),
            }
            assert!(decode_bundle(&bytes, &expected, &DecodeOptions::default()).is_err());
        }
        assert!(decode_bundle(
            &edit(&bytes, |b| b.block_access = None),
            &expectation(),
            &DecodeOptions::default()
        )
        .is_err());
        assert!(decode_bundle(
            &edit(&bytes, |b| b.version = 2),
            &expectation(),
            &DecodeOptions::default()
        )
        .is_err());
    }
    #[test]
    fn truncated_trailing_and_forged_signature_data_are_rejected() {
        let bytes = fixture(0, false, true);
        assert!(decode_bundle(
            &bytes[..bytes.len() - 1],
            &expectation(),
            &DecodeOptions::default()
        )
        .is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_bundle(&trailing, &expectation(), &DecodeOptions::default()).is_err());
        let bad = edit(&bytes, |b| {
            let mut a: ArchiveV2BlockAccessBlob = wincode::config::deserialize_exact(
                b.block_access.as_ref().unwrap(),
                wincode_leb128_config(),
            )
            .unwrap();
            a.signature_counts[0] = 2;
            b.block_access = Some(wincode::config::serialize(&a, wincode_leb128_config()).unwrap());
        });
        assert!(decode_bundle(&bad, &expectation(), &DecodeOptions::default()).is_err());
    }
    #[test]
    fn sparse_key_ids_use_the_full_access_map_without_dense_allocation() {
        let mut keys = AccessKeys(BTreeMap::new());
        keys.0.insert(u32::MAX, [7; 32]);
        assert_eq!(
            key_bytes(CompactPubkey::Id(u32::MAX), &keys).unwrap(),
            [7; 32]
        );
        assert!(key_bytes(CompactPubkey::Id(0), &keys).is_err());
        assert!(key_bytes(CompactPubkey::Id(1), &keys).is_err());
    }

    #[test]
    fn compressed_frames_obey_the_decoded_and_window_limits() {
        // The encoder uses a 128 KiB window even for this short payload.
        const WINDOW: usize = 128 * 1024;
        let bytes = ruzstd::encoding::compress_to_vec(
            vec![0u8; 4096].as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        assert!(unpack(BlockzillaGetBlockBlobEncoding::ZstdWincode, &bytes, 8192).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(unpack(
            BlockzillaGetBlockBlobEncoding::ZstdWincode,
            &trailing,
            WINDOW
        )
        .is_err());
        assert_eq!(
            unpack(BlockzillaGetBlockBlobEncoding::ZstdWincode, &bytes, WINDOW)
                .unwrap()
                .len(),
            4096
        );

        // The window is admitted, but decoded output must still fit the bound.
        let oversized = ruzstd::encoding::compress_to_vec(
            vec![0u8; WINDOW * 2].as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let error = unpack(
            BlockzillaGetBlockBlobEncoding::ZstdWincode,
            &oversized,
            WINDOW,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("decoded byte limit"),
            "{error:#}"
        );
    }
}
