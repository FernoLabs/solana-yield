//! Bounded range membership from an operator-admitted native epoch index.
//! This proves membership in the pinned object set, not canonical publication.
use anyhow::{ensure, Context, Result};
use blockzilla_archive_v2::read_archive_v2_hot_block_index_file;
use serde::Serialize;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_RANGE_SLOTS: u64 = 4096;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedRange {
    pub produced_slots: Vec<u64>,
    pub skipped_slots: Vec<u64>,
    pub source_index_sha256: String,
    pub source_blob_file_bytes: u64,
    pub total_index_rows: usize,
}

pub fn read_indexed_range(path: &Path, sha256: &str, start: u64, end: u64) -> Result<IndexedRange> {
    ensure!(
        start <= end && end - start < MAX_RANGE_SLOTS,
        "archive range exceeds slot limit"
    );
    let mut file = File::open(path).context("open admitted native archive index")?;
    ensure!(
        file.metadata()?.len() <= MAX_INDEX_BYTES,
        "archive index exceeds byte limit"
    );
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_INDEX_BYTES && crate::digest(&bytes) == sha256,
        "archive index digest differs from admission"
    );
    file.seek(SeekFrom::Start(0))?;
    let index = read_archive_v2_hot_block_index_file(file, path)?;
    ensure!(
        !index.rows.is_empty() && index.rows.len() <= 1_000_000,
        "invalid archive index row count"
    );
    ensure!(
        index.rows[0].slot <= start && index.rows.last().unwrap().slot >= end,
        "range extends beyond indexed source boundaries"
    );
    let (mut offset, mut transactions, mut signatures) = (0u64, 0u64, 0u64);
    let mut previous_slot = None;
    let mut produced = Vec::new();
    for (position, row) in index.rows.iter().enumerate() {
        ensure!(
            row.block_id as usize == position
                && row.compressed_offset == offset
                && row.first_tx_ordinal == transactions
                && row.first_signature_ordinal == signatures,
            "noncontiguous admitted archive index"
        );
        ensure!(
            previous_slot.is_none_or(|slot| row.slot > slot),
            "archive index slots are not increasing"
        );
        ensure!(
            row.compressed_len > 0
                && row.uncompressed_len > 0
                && row.uncompressed_len <= 64 * 1024 * 1024
                && row.tx_count <= 65_536,
            "archive index block exceeds limit"
        );
        offset = offset
            .checked_add(u64::from(row.compressed_len))
            .context("index offset overflow")?;
        transactions = transactions
            .checked_add(u64::from(row.tx_count))
            .context("index transaction overflow")?;
        signatures = signatures
            .checked_add(u64::from(row.signature_count))
            .context("index signature overflow")?;
        previous_slot = Some(row.slot);
        if start <= row.slot && row.slot <= end {
            produced.push(row.slot);
        }
    }
    ensure!(
        offset == index.blob_file_bytes,
        "archive index does not cover its declared blob"
    );
    let skipped = (start..=end)
        .filter(|slot| produced.binary_search(slot).is_err())
        .collect();
    Ok(IndexedRange {
        produced_slots: produced,
        skipped_slots: skipped,
        source_index_sha256: sha256.to_owned(),
        source_blob_file_bytes: index.blob_file_bytes,
        total_index_rows: index.rows.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockzilla_archive_v2::{write_archive_v2_hot_block_index, ArchiveV2HotBlockIndexRow};
    #[test]
    fn native_index_proves_skips_and_rejects_changed_admission_or_row_order() {
        let path =
            std::env::temp_dir().join(format!("ferno-range-index-{}.bin", std::process::id()));
        let rows: Vec<_> = [100, 101, 103, 104]
            .into_iter()
            .enumerate()
            .map(|(i, slot)| ArchiveV2HotBlockIndexRow {
                block_id: i as u32,
                slot,
                compressed_offset: i as u64,
                compressed_len: 1,
                uncompressed_len: 1,
                tx_count: 0,
                first_tx_ordinal: 0,
                first_signature_ordinal: 0,
                signature_count: 0,
            })
            .collect();
        write_archive_v2_hot_block_index(&path, 4, 0, 0, &rows).unwrap();
        let digest = crate::digest(&std::fs::read(&path).unwrap());
        let range = read_indexed_range(&path, &digest, 101, 103).unwrap();
        assert_eq!(range.produced_slots, [101, 103]);
        assert_eq!(range.skipped_slots, [102]);
        assert!(read_indexed_range(&path, "wrong", 101, 103).is_err());
        assert!(read_indexed_range(&path, &digest, 99, 103).is_err());
        let mut wrong = rows;
        wrong[2].slot = 101;
        write_archive_v2_hot_block_index(&path, 4, 0, 0, &wrong).unwrap();
        let digest = crate::digest(&std::fs::read(&path).unwrap());
        assert!(read_indexed_range(&path, &digest, 101, 103).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
