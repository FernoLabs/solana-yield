//! Produced-parent continuity is separate from intake sequence and exact-bank finality.
use anyhow::{ensure, Context, Result};
use blockzilla_bank_client::{BankIdentity, Chain};
use blockzilla_bank_stream_v2::chain::BankState;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootAnchor {
    pub bank: BankIdentity,
    pub block_height: u64,
    pub payload_frame_sha256: String,
    pub root_cursor: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingParent {
    pub child: BankIdentity,
    pub parent_slot: u64,
    pub parent_blockhash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContinuityReport {
    pub status: String,
    pub capture: BankState,
    pub verified_root: Option<RootAnchor>,
    pub highest_root: Option<RootAnchor>,
    pub gap_reason: Option<String>,
    pub missing_parent: Option<MissingParent>,
    pub newly_verified_payloads: Vec<String>,
}
fn key(bank: &BankIdentity) -> String {
    format!("{}:{}", bank.slot, bank.bank_id)
}
fn capture_valid(chain: &Chain, capture: &BankState) -> Result<()> {
    capture.bank.validate()?;
    ensure!(
        (1..=blockzilla_bank_stream_v2::SAFE).contains(&capture.block_height)
            && blockzilla_bank_stream_v2::is_digest(&capture.payload_sha256)
            && blockzilla_bank_stream_v2::is_digest(&capture.payload_frame_sha256),
        "capture has no valid parent height"
    );
    let state = chain
        .banks
        .get(&key(&capture.bank))
        .context("capture bank evidence missing")?;
    ensure!(
        state.bank == capture.bank
            && state.block_height == capture.block_height
            && state.payload_sha256 == capture.payload_sha256
            && state.payload_frame_sha256 == capture.payload_frame_sha256,
        "capture bank evidence changed"
    );
    Ok(())
}
fn anchor_valid(
    chain: &Chain,
    anchor: &RootAnchor,
    roots: &BTreeMap<String, String>,
) -> Result<()> {
    let state = chain
        .banks
        .get(&key(&anchor.bank))
        .context("saved root evidence missing")?;
    ensure!(
        state.bank == anchor.bank
            && state.block_height == anchor.block_height
            && state.payload_frame_sha256 == anchor.payload_frame_sha256
            && state.rooted
            && state.confirmed
            && !state.invalidated,
        "saved root evidence changed"
    );
    ensure!(
        roots.get(&anchor.payload_frame_sha256) == Some(&anchor.root_cursor),
        "saved native root control changed"
    );
    root_cursor_valid(&anchor.root_cursor)?;
    Ok(())
}
fn root_cursor_valid(cursor: &str) -> Result<()> {
    let (_, sequence, digest) = blockzilla_bank_client::parse_cursor(cursor)?;
    ensure!(
        sequence > 0 && blockzilla_bank_stream_v2::is_digest(digest),
        "native root cursor has no frame evidence"
    );
    Ok(())
}
fn from_capture(
    chain: &Chain,
    capture: &BankState,
    root: &BankIdentity,
    eligible: &BTreeSet<String>,
) -> Result<Vec<BankState>> {
    let path = chain.rooted_path_from_boundary(
        root,
        capture.bank.parent_slot,
        capture.bank.parent_blockhash,
        capture.block_height - 1,
    )?;
    ensure!(
        path.first().is_some_and(|first| first.bank == capture.bank
            && first.payload_frame_sha256 == capture.payload_frame_sha256),
        "rooted path does not include the exact captured bank"
    );
    ensure!(
        path.iter()
            .all(|bank| eligible.contains(&bank.payload_frame_sha256)),
        "rooted path has no complete parsed evidence"
    );
    Ok(path)
}
/// Validate a durable anchor against the entire retained path after restart.
/// A valid anchor is not replaced with a newer provider head.
pub fn validate_restore(
    chain: &Chain,
    capture: &BankState,
    previous: Option<&RootAnchor>,
    roots: &BTreeMap<String, String>,
    eligible: &BTreeSet<String>,
) -> Result<()> {
    capture_valid(chain, capture)?;
    if let Some(previous) = previous {
        anchor_valid(chain, previous, roots)?;
        from_capture(chain, capture, &previous.bank, eligible)?;
    }
    Ok(())
}
/// Recompute derived report fields. Saved flags are not continuity evidence.
pub fn validate_saved_report(
    chain: &Chain,
    capture: &BankState,
    saved: &ContinuityReport,
    roots: &BTreeMap<String, String>,
    eligible: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    validate_restore(
        chain,
        capture,
        saved.verified_root.as_ref(),
        roots,
        eligible,
    )?;
    let current = evaluate(
        chain,
        capture,
        saved.verified_root.as_ref(),
        roots,
        eligible,
    )?;
    ensure!(
        saved.status == current.status
            && saved.verified_root == current.verified_root
            && saved.highest_root == current.highest_root
            && saved.gap_reason == current.gap_reason
            && saved.missing_parent == current.missing_parent,
        "saved continuity report does not match admitted evidence"
    );
    let path = saved
        .verified_root
        .as_ref()
        .map(|anchor| from_capture(chain, capture, &anchor.bank, eligible))
        .transpose()?
        .unwrap_or_default();
    let covered: BTreeSet<String> = path
        .into_iter()
        .map(|bank| bank.payload_frame_sha256)
        .collect();
    let delta: BTreeSet<_> = saved.newly_verified_payloads.iter().collect();
    ensure!(
        delta.len() == saved.newly_verified_payloads.len()
            && delta.iter().all(|digest| covered.contains(*digest)),
        "saved continuity delta is outside verified coverage"
    );
    Ok(covered)
}
/// A missing produced parent or invalid ancestry leaves the verified root fixed.
/// Slot jumps are allowed: only the exact parent slot/hash and native height link
/// matter. This proves continuity in admitted rooted evidence, not independent
/// consensus or complete CPI/account-write recording.
pub fn evaluate(
    chain: &Chain,
    capture: &BankState,
    previous: Option<&RootAnchor>,
    roots: &BTreeMap<String, String>,
    eligible: &BTreeSet<String>,
) -> Result<ContinuityReport> {
    capture_valid(chain, capture)?;
    if let Some(previous) = previous {
        anchor_valid(chain, previous, roots)?;
        // Never commit evidence that makes an already verified prefix invalid.
        from_capture(chain, capture, &previous.bank, eligible)?;
    }
    let mut report = ContinuityReport {
        status: if previous.is_some() {
            "contiguous"
        } else {
            "awaiting_root"
        }
        .into(),
        capture: capture.clone(),
        verified_root: previous.cloned(),
        highest_root: None,
        gap_reason: None,
        missing_parent: None,
        newly_verified_payloads: Vec::new(),
    };
    let mut candidates: Vec<_> = chain
        .banks
        .values()
        .filter(|bank| bank.rooted && bank.block_height >= capture.block_height)
        .collect();
    candidates.sort_by_key(|bank| bank.block_height);
    let Some(root) = candidates.last().copied() else {
        return Ok(report);
    };
    if candidates
        .iter()
        .filter(|bank| bank.block_height == root.block_height)
        .count()
        > 1
    {
        report.status = "gap".into();
        report.gap_reason = Some("ambiguous rooted bank at the same native height".into());
        return Ok(report);
    }
    let root_cursor = roots
        .get(&root.payload_frame_sha256)
        .context("rooted bank has no exact native control")?
        .clone();
    root_cursor_valid(&root_cursor)?;
    let highest = RootAnchor {
        bank: root.bank.clone(),
        block_height: root.block_height,
        payload_frame_sha256: root.payload_frame_sha256.clone(),
        root_cursor,
    };
    report.highest_root = Some(highest.clone());
    let path = from_capture(chain, capture, &root.bank, eligible).and_then(|full_path| {
        // A late control on a conflicting older branch must not disappear below
        // the saved root. Every captured root must be on the same complete path.
        for rooted in &candidates {
            ensure!(
                full_path.iter().any(|bank| bank.bank == rooted.bank
                    && bank.payload_frame_sha256 == rooted.payload_frame_sha256),
                "conflicting native root inside the captured range"
            );
            root_cursor_valid(
                roots
                    .get(&rooted.payload_frame_sha256)
                    .context("rooted bank has no exact native control")?,
            )?;
        }
        if let Some(previous) = previous {
            chain
                .rooted_path_after(&root.bank, &previous.bank, previous.block_height)
                .and_then(|path| {
                    ensure!(
                        path.iter()
                            .all(|bank| eligible.contains(&bank.payload_frame_sha256)),
                        "rooted path has no complete parsed evidence"
                    );
                    Ok(path)
                })
        } else {
            Ok(full_path)
        }
    });
    match path {
        Ok(path) => {
            report.status = "contiguous".into();
            report.verified_root = Some(highest);
            report.newly_verified_payloads = path
                .into_iter()
                .map(|bank| bank.payload_frame_sha256)
                .collect();
        }
        Err(error) => {
            report.status = "gap".into();
            report.gap_reason = Some(error.to_string());
            report.missing_parent = find_missing_parent(
                chain,
                &root.bank,
                previous.map(|anchor| &anchor.bank).unwrap_or(&capture.bank),
            );
        }
    }
    Ok(report)
}
fn find_missing_parent(
    chain: &Chain,
    root: &BankIdentity,
    anchor: &BankIdentity,
) -> Option<MissingParent> {
    let mut current = root.clone();
    let mut seen = BTreeSet::new();
    while current != *anchor && seen.insert(key(&current)) {
        let parents: Vec<_> = chain
            .banks
            .values()
            .filter(|state| current.parent_matches(&state.bank) && !state.invalidated)
            .collect();
        match parents.as_slice() {
            [] => {
                return Some(MissingParent {
                    parent_slot: current.parent_slot,
                    parent_blockhash: bs58::encode(current.parent_blockhash).into_string(),
                    child: current,
                })
            }
            [parent] => current = parent.bank.clone(),
            _ => return None,
        }
    }
    None
}
