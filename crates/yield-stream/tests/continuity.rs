//! Continuity follows produced parents, including parents with no parser match.
//! These synthetic chain records require no stream, private key, or RPC call.
use blockzilla_bank_client::{BankIdentity, Chain};
use blockzilla_bank_stream_v2::chain::BankState;
use std::collections::{BTreeMap, BTreeSet};
use yield_stream::continuity::{
    evaluate, validate_restore, validate_saved_report, MissingParent, RootAnchor,
};

fn state(
    slot: u64,
    id: u64,
    parent_slot: u64,
    parent_hash: [u8; 32],
    height: u64,
    rooted: bool,
) -> BankState {
    BankState {
        bank: BankIdentity {
            slot,
            bank_id: id,
            blockhash: [id as u8; 32],
            parent_slot,
            parent_blockhash: parent_hash,
        },
        payload_sha256: format!("{:064x}", id * 100 + 1),
        payload_frame_sha256: format!("{:064x}", id * 100 + 2),
        block_height: height,
        invalidated: false,
        rooted,
        confirmed: rooted,
    }
}
fn capture() -> BankState {
    state(100, 1, 99, [99; 32], 80, false)
}
fn child(parent: &BankState, slot: u64, id: u64, rooted: bool) -> BankState {
    state(
        slot,
        id,
        parent.bank.slot,
        parent.bank.blockhash,
        parent.block_height + 1,
        rooted,
    )
}
fn bank_key(state: &BankState) -> String {
    format!("{}:{}", state.bank.slot, state.bank.bank_id)
}
fn root_cursor(state: &BankState) -> String {
    format!(
        "continuity-session:p{}:{:064x}",
        state.bank.bank_id,
        state.bank.bank_id + 900
    )
}
fn anchor(state: &BankState) -> RootAnchor {
    RootAnchor {
        bank: state.bank.clone(),
        block_height: state.block_height,
        payload_frame_sha256: state.payload_frame_sha256.clone(),
        root_cursor: root_cursor(state),
    }
}
fn inputs(states: &[BankState]) -> (Chain, BTreeMap<String, String>, BTreeSet<String>) {
    let mut chain = Chain::default();
    let mut cursors = BTreeMap::new();
    let mut eligible = BTreeSet::new();
    for state in states {
        chain.banks.insert(bank_key(state), state.clone());
        if state.rooted {
            cursors.insert(state.payload_frame_sha256.clone(), root_cursor(state));
        }
        eligible.insert(state.payload_frame_sha256.clone());
    }
    (chain, cursors, eligible)
}

#[test]
fn skipped_slot_jump_is_contiguous_when_parent_and_height_match() {
    let start = capture();
    let root = child(&start, 104, 2, true);
    let (chain, cursors, eligible) = inputs(&[start.clone(), root.clone()]);
    let report = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(report.status, "contiguous");
    assert_eq!(report.verified_root.as_ref().unwrap().bank, root.bank);
    assert_eq!(report.highest_root.as_ref().unwrap().bank, root.bank);
    assert!(report.gap_reason.is_none());
    assert!(report.missing_parent.is_none());
    assert_eq!(
        report.newly_verified_payloads,
        [start.payload_frame_sha256, root.payload_frame_sha256]
    );
}

#[test]
fn an_omitted_produced_parent_preserves_the_saved_anchor_and_reports_exact_dependency() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let middle = child(&start, 104, 2, false);
    let root = child(&middle, 108, 3, true);
    let previous = anchor(&start);
    let (chain, cursors, eligible) = inputs(&[start.clone(), root.clone()]);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.as_ref().unwrap().bank, start.bank);
    assert_eq!(report.highest_root.as_ref().unwrap().bank, root.bank);
    assert!(report.gap_reason.is_some());
    let missing = report.missing_parent.unwrap();
    assert_eq!(missing.child, root.bank);
    assert_eq!(missing.parent_slot, middle.bank.slot);
    assert_eq!(
        missing.parent_blockhash,
        bs58::encode(middle.bank.blockhash).into_string()
    );
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn a_late_parent_repairs_the_path_without_changing_the_saved_anchor() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let middle = child(&start, 104, 2, false);
    let root = child(&middle, 108, 3, true);
    let previous = anchor(&start);
    let (mut chain, cursors, mut eligible) = inputs(&[start.clone(), root.clone()]);
    assert_eq!(
        evaluate(&chain, &start, Some(&previous), &cursors, &eligible)
            .unwrap()
            .status,
        "gap"
    );
    chain.banks.insert(bank_key(&middle), middle.clone());
    eligible.insert(middle.payload_frame_sha256.clone());
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "contiguous");
    assert_eq!(report.verified_root.unwrap().bank, root.bank);
    assert_eq!(
        report.newly_verified_payloads,
        [middle.payload_frame_sha256, root.payload_frame_sha256]
    );
    assert_eq!(previous.bank, start.bank);
}

#[test]
fn the_same_parent_slot_with_a_different_hash_does_not_connect_the_chain() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let mut root = child(&start, 104, 2, true);
    root.bank.parent_blockhash = [7; 32];
    let previous = anchor(&start);
    let (chain, cursors, eligible) = inputs(&[start.clone(), root]);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.unwrap().bank, start.bank);
    let missing = report.missing_parent.unwrap();
    assert_eq!(missing.parent_slot, start.bank.slot);
    assert_eq!(
        missing.parent_blockhash,
        bs58::encode([7; 32]).into_string()
    );
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn ambiguous_parent_bank_ids_with_the_same_hash_block_progress() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let middle = child(&start, 104, 2, false);
    let mut duplicate = middle.clone();
    duplicate.bank.bank_id = 9;
    duplicate.payload_frame_sha256 = format!("{:064x}", 999);
    let root = child(&middle, 108, 3, true);
    let previous = anchor(&start);
    let (chain, cursors, eligible) = inputs(&[start.clone(), middle, duplicate, root]);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.unwrap().bank, start.bank);
    assert!(report.gap_reason.is_some());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn an_intermediate_root_cannot_hide_an_uncaptured_produced_parent() {
    let start = capture();
    let absent = child(&start, 102, 2, false);
    let middle_root = child(&absent, 104, 3, true);
    let root = child(&middle_root, 108, 4, true);
    let (chain, cursors, eligible) = inputs(&[start.clone(), middle_root.clone(), root.clone()]);
    let report = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert!(report.verified_root.is_none());
    assert_eq!(report.highest_root.unwrap().bank, root.bank);
    let missing = report.missing_parent.unwrap();
    assert_eq!(missing.child, middle_root.bank);
    assert_eq!(missing.parent_slot, absent.bank.slot);
    assert_eq!(
        missing.parent_blockhash,
        bs58::encode(absent.bank.blockhash).into_string()
    );
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn the_first_root_path_must_include_the_exact_capture_bank_not_its_sibling() {
    let start = capture();
    let sibling = state(100, 9, 99, [99; 32], 80, false);
    let root = child(&sibling, 104, 2, true);
    let (chain, cursors, eligible) = inputs(&[start.clone(), sibling, root]);
    let report = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert!(report.verified_root.is_none());
    assert!(report.gap_reason.is_some());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn invalidated_ancestry_cannot_advance_finalized_coverage() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let mut middle = child(&start, 104, 2, false);
    middle.invalidated = true;
    let root = child(&middle, 108, 3, true);
    let previous = anchor(&start);
    let (chain, cursors, eligible) = inputs(&[start.clone(), middle, root]);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.unwrap().bank, start.bank);
    assert!(report.gap_reason.is_some());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn wrong_native_parent_height_blocks_progress_even_when_slot_and_hash_match() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let mut root = child(&start, 104, 2, true);
    root.block_height += 1;
    let previous = anchor(&start);
    let (chain, cursors, eligible) = inputs(&[start.clone(), root]);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.unwrap().bank, start.bank);
    assert!(report.gap_reason.is_some());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn restore_rejects_changed_saved_anchor_identity_height_or_payload_digest() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let (chain, cursors, eligible) = inputs(&[start.clone()]);
    let original = anchor(&start);
    validate_restore(&chain, &start, Some(&original), &cursors, &eligible).unwrap();
    for mutation in 0..3 {
        let mut changed = anchor(&start);
        match mutation {
            0 => changed.bank.blockhash = [7; 32],
            1 => changed.block_height += 1,
            _ => changed.payload_frame_sha256 = format!("{:064x}", 999),
        }
        assert!(validate_restore(&chain, &start, Some(&changed), &cursors, &eligible).is_err());
        assert!(evaluate(&chain, &start, Some(&changed), &cursors, &eligible).is_err());
    }
}

#[test]
fn capture_identity_and_height_are_immutable_but_status_flags_can_advance() {
    let start = capture();
    let mut rooted_capture = start.clone();
    rooted_capture.rooted = true;
    rooted_capture.confirmed = true;
    let (chain, cursors, eligible) = inputs(&[rooted_capture]);
    validate_restore(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(
        evaluate(&chain, &start, None, &cursors, &eligible)
            .unwrap()
            .status,
        "contiguous"
    );
    for mutation in 0..3 {
        let mut changed = start.clone();
        match mutation {
            0 => changed.bank.blockhash = [7; 32],
            1 => changed.block_height += 1,
            _ => changed.payload_frame_sha256 = format!("{:064x}", 999),
        }
        assert!(validate_restore(&chain, &changed, None, &cursors, &eligible).is_err());
    }
}

#[test]
fn restore_requires_the_entire_saved_root_path_back_to_capture() {
    let start = capture();
    let middle = child(&start, 104, 2, true);
    let root = child(&middle, 108, 3, true);
    let previous = anchor(&root);
    let (mut chain, cursors, eligible) = inputs(&[start.clone(), middle.clone(), root]);
    validate_restore(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    chain.banks.remove(&bank_key(&middle));
    assert!(validate_restore(&chain, &start, Some(&previous), &cursors, &eligible).is_err());
}

#[test]
fn older_roots_below_capture_do_not_establish_new_capture_coverage() {
    let start = capture();
    let older_root = state(96, 8, 95, [95; 32], 78, true);
    let (chain, cursors, eligible) = inputs(&[start.clone(), older_root]);
    let report = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(report.status, "awaiting_root");
    assert!(report.verified_root.is_none());
    assert!(report.highest_root.is_none());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn an_uncommitted_parent_payload_blocks_coverage_even_if_present_in_sdk_chain() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let middle = child(&start, 104, 2, false);
    let root = child(&middle, 108, 3, true);
    let previous = anchor(&start);
    let (chain, cursors, mut eligible) = inputs(&[start.clone(), middle.clone(), root]);
    eligible.remove(&middle.payload_frame_sha256);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.unwrap().bank, start.bank);
    assert!(report.gap_reason.is_some());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn a_processed_path_waits_for_native_root_evidence() {
    let start = capture();
    let next = child(&start, 104, 2, false);
    let (chain, cursors, eligible) = inputs(&[start.clone(), next]);
    let report = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(report.status, "awaiting_root");
    assert!(report.verified_root.is_none());
    assert!(report.highest_root.is_none());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn restore_rejects_malformed_capture_digests_even_if_the_chain_record_matches() {
    for (payload_digest, frame_digest) in [
        ("not-a-digest".to_owned(), format!("{:064x}", 102)),
        (format!("{:064x}", 101), "not-a-digest".to_owned()),
        ("A".repeat(64), format!("{:064x}", 102)),
        (format!("{:064x}", 101), "f".repeat(63)),
    ] {
        let mut start = capture();
        start.payload_sha256 = payload_digest;
        start.payload_frame_sha256 = frame_digest;
        let (chain, cursors, eligible) = inputs(&[start.clone()]);
        assert!(validate_restore(&chain, &start, None, &cursors, &eligible).is_err());
        assert!(evaluate(&chain, &start, None, &cursors, &eligible).is_err());
    }
}

#[test]
fn restore_rejects_capture_height_above_the_protocol_safe_integer_bound() {
    let mut start = capture();
    start.block_height = 9_007_199_254_740_992;
    let (chain, cursors, eligible) = inputs(&[start.clone()]);
    assert!(validate_restore(&chain, &start, None, &cursors, &eligible).is_err());
    assert!(evaluate(&chain, &start, None, &cursors, &eligible).is_err());
}

#[test]
fn a_zero_sequence_bootstrap_cursor_cannot_prove_a_native_root() {
    let mut start = capture();
    start.rooted = true;
    start.confirmed = true;
    let (chain, mut cursors, eligible) = inputs(&[start.clone()]);
    let bootstrap = "continuity-session:p0:-".to_owned();
    cursors.insert(start.payload_frame_sha256.clone(), bootstrap.clone());
    let mut previous = anchor(&start);
    previous.root_cursor = bootstrap;
    assert!(validate_restore(&chain, &start, Some(&previous), &cursors, &eligible).is_err());
    assert!(evaluate(&chain, &start, Some(&previous), &cursors, &eligible).is_err());
    assert!(evaluate(&chain, &start, None, &cursors, &eligible).is_err());
}

#[test]
fn a_late_conflicting_root_below_the_saved_root_blocks_coverage_progress() {
    let start = capture();
    let middle = child(&start, 104, 2, false);
    let latest = child(&middle, 108, 3, true);
    let (mut chain, mut cursors, mut eligible) = inputs(&[start.clone(), middle, latest.clone()]);
    let first = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    assert_eq!(first.status, "contiguous");
    let previous = first.verified_root.unwrap();
    let competing_root = child(&start, 104, 9, true);
    chain
        .banks
        .insert(bank_key(&competing_root), competing_root.clone());
    cursors.insert(
        competing_root.payload_frame_sha256.clone(),
        root_cursor(&competing_root),
    );
    eligible.insert(competing_root.payload_frame_sha256);
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "gap");
    assert_eq!(report.verified_root.unwrap(), previous);
    assert_eq!(previous.bank, latest.bank);
    assert!(report.gap_reason.is_some());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn a_late_native_root_on_the_saved_verified_path_keeps_coverage_contiguous() {
    let start = capture();
    let mut middle = child(&start, 104, 2, false);
    let latest = child(&middle, 108, 3, true);
    let (mut chain, mut cursors, eligible) =
        inputs(&[start.clone(), middle.clone(), latest.clone()]);
    let first = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    let previous = first.verified_root.unwrap();
    middle.rooted = true;
    middle.confirmed = true;
    chain.banks.insert(bank_key(&middle), middle.clone());
    cursors.insert(middle.payload_frame_sha256.clone(), root_cursor(&middle));
    let report = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(report.status, "contiguous");
    assert_eq!(report.verified_root.unwrap(), previous);
    assert_eq!(previous.bank, latest.bank);
    assert!(report.gap_reason.is_none());
    assert!(report.missing_parent.is_none());
    assert!(report.newly_verified_payloads.is_empty());
}

#[test]
fn saved_report_validation_rejects_changed_status_highest_root_or_missing_parent() {
    let start = capture();
    let root = child(&start, 104, 2, true);
    let (chain, cursors, eligible) = inputs(&[start.clone(), root.clone()]);
    let original = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    let covered = validate_saved_report(&chain, &start, &original, &cursors, &eligible).unwrap();
    assert_eq!(
        covered,
        BTreeSet::from([
            start.payload_frame_sha256.clone(),
            root.payload_frame_sha256.clone(),
        ])
    );
    for mutation in 0..3 {
        let mut changed = original.clone();
        match mutation {
            0 => changed.status = "awaiting_root".into(),
            1 => changed.highest_root = None,
            _ => {
                changed.missing_parent = Some(MissingParent {
                    child: root.bank.clone(),
                    parent_slot: start.bank.slot,
                    parent_blockhash: bs58::encode(start.bank.blockhash).into_string(),
                });
            }
        }
        assert!(validate_saved_report(&chain, &start, &changed, &cursors, &eligible).is_err());
    }
}

#[test]
fn a_saved_gap_report_restores_only_the_exact_verified_prefix() {
    let start = capture();
    let middle = child(&start, 104, 2, false);
    let latest = child(&middle, 108, 3, true);
    let (mut chain, mut cursors, mut eligible) =
        inputs(&[start.clone(), middle.clone(), latest.clone()]);
    let first = evaluate(&chain, &start, None, &cursors, &eligible).unwrap();
    let previous = first.verified_root.unwrap();
    let absent = child(&latest, 112, 4, false);
    let disconnected_root = child(&absent, 116, 5, true);
    chain
        .banks
        .insert(bank_key(&disconnected_root), disconnected_root.clone());
    cursors.insert(
        disconnected_root.payload_frame_sha256.clone(),
        root_cursor(&disconnected_root),
    );
    eligible.insert(disconnected_root.payload_frame_sha256.clone());
    let gap = evaluate(&chain, &start, Some(&previous), &cursors, &eligible).unwrap();
    assert_eq!(gap.status, "gap");
    assert_eq!(gap.verified_root.as_ref().unwrap(), &previous);
    let covered = validate_saved_report(&chain, &start, &gap, &cursors, &eligible).unwrap();
    assert_eq!(
        covered,
        BTreeSet::from([
            start.payload_frame_sha256,
            middle.payload_frame_sha256,
            latest.payload_frame_sha256,
        ])
    );
    assert!(!covered.contains(&absent.payload_frame_sha256));
    assert!(!covered.contains(&disconnected_root.payload_frame_sha256));
}
