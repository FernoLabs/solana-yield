//! The legacy chunk wire contract. This module has no HTTP or runtime dependency.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;

pub const MAX_META: usize = 64 * 1024;
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
pub const MAX_FRAME: usize = 16 + MAX_META + MAX_PAYLOAD;
const SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub session: String,
    pub slot: u64,
    pub digest: String,
}

pub fn parse_cursor(text: &str) -> Result<Cursor> {
    let parts: Vec<_> = text.split(':').collect();
    ensure!(parts.len() == 3, "invalid Blockzilla cursor");
    ensure!(
        !parts[0].is_empty()
            && parts[0].len() <= 128
            && parts[0]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid Blockzilla session"
    );
    ensure!(
        parts[2] == "-"
            || (parts[2].len() == 64
                && parts[2]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
        "invalid Blockzilla digest"
    );
    let slot: u64 = parts[1].parse().context("invalid Blockzilla slot")?;
    ensure!(
        slot <= SAFE_INTEGER && parts[1] == slot.to_string(),
        "invalid Blockzilla slot"
    );
    Ok(Cursor {
        session: parts[0].into(),
        slot,
        digest: parts[2].into(),
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkNotice {
    pub cursor: String,
    pub sha256: String,
    pub bytes: usize,
    pub start_slot: u64,
    pub end_slot: u64,
    pub url: String,
}
impl ChunkNotice {
    pub fn validate(&self, event_id: &str) -> Result<()> {
        let cursor = parse_cursor(&self.cursor)?;
        ensure!(
            event_id == self.cursor
                && cursor.digest == self.sha256
                && cursor.digest != "-"
                && (16..=MAX_FRAME).contains(&self.bytes)
                && self.start_slot <= self.end_slot
                && self.end_slot == cursor.slot
                && self.url == format!("/v1/chunks/{}", self.sha256),
            "invalid Blockzilla chunk notice"
        );
        Ok(())
    }
}

/// Adapt a transport-specific notice without adding a transport dependency.
/// The decoder validates the resulting fields before it reads a chunk.
pub trait ChunkNoticeSource {
    fn to_chunk_notice(&self) -> ChunkNotice;
}

impl ChunkNoticeSource for ChunkNotice {
    fn to_chunk_notice(&self) -> ChunkNotice {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_contract_rejects_noncanonical_cursor_and_notice() {
        for cursor in [
            "test:01:-",
            "test:+1:-",
            "test:9007199254740992:-",
            "bad_session:1:-",
            "test:1:bad",
            "test:1:-:extra",
            ":1:-",
        ] {
            assert!(parse_cursor(cursor).is_err(), "{cursor}");
        }
        let digest = "a".repeat(64);
        let mut notice = ChunkNotice {
            cursor: format!("test:100:{digest}"),
            sha256: digest.clone(),
            bytes: 16,
            start_slot: 99,
            end_slot: 100,
            url: format!("/v1/chunks/{digest}"),
        };
        notice.validate(&notice.cursor).unwrap();
        assert!(notice.validate("test:99:-").is_err());
        notice.url.push('/');
        assert!(notice.validate(&notice.cursor).is_err());
    }
}
