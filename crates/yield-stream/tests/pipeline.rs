//! The real native-compact decoder, local program selection, and protocol
//! parser run together. Inputs are synthetic bank frames; no RPC is used.
use base64::{engine::general_purpose::STANDARD, Engine};
use blockzilla_decoder::tests::bank_fixture_with_program_logs;
use yield_indexer::stream_parser::{JupiterEvent, JUPITER_LIQUIDITY_PROGRAM};
use yield_stream::pipeline::parse_block;

const OTHER: &str = "jupr81YtYssSyPt8jbnGuiWon5f6x9TcDEFxYe3Bdzi";
// Public local-fork event recorded in
// packages/sdk/src/fixtures/carry-switch-receipts-fork-2026-09-14.json.
// It proves decoding, not live mainnet coverage or current yield.
const RECORDED_OPERATE: &str = "tAhRRxOErQhEOX5oO2595aSDxpiEgZ0cyWoqkwBD2Y83QoyhbWSFOxwu5n8STfbt8qnSv146Uzx3AbWPjaqVchCh5bBhjNK+AAAAAAAAAAAAAAAAAAAAAHVpZ/////////////////9l9d/9fISvdail973OoWIuKzq/h/xTqHG5CFYaOosb3mX13/18hK91qKX3vc6hYi4rOr+H/FOocbkIVho6ixveqEo0IvUAAADi817K+QAAAA==";

fn key(program: &str) -> [u8; 32] {
    bs58::decode(program)
        .into_vec()
        .unwrap()
        .try_into()
        .unwrap()
}

fn invoke(program: &str, depth: usize) -> String {
    format!("Program {program} invoke [{depth}]")
}

fn success(program: &str) -> String {
    format!("Program {program} success")
}

fn operate_log() -> String {
    format!("Program data: {RECORDED_OPERATE}")
}

fn logs(program: &str) -> Vec<String> {
    vec![invoke(program, 1), operate_log(), success(program)]
}

#[test]
fn native_bank_selects_jupiter_locally_and_keeps_original_transaction_position() {
    let frame = bank_fixture_with_program_logs(
        key(JUPITER_LIQUIDITY_PROGRAM),
        false,
        Some(logs(JUPITER_LIQUIDITY_PROGRAM)),
        3,
        false,
    );
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_scanned, 4);
    assert_eq!(parsed.transactions_selected, 1);
    assert_eq!(parsed.transactions_succeeded, 1);
    assert_eq!(parsed.transactions_failed, 0);
    assert_eq!(parsed.transactions_without_metadata, 0);
    assert_eq!(parsed.parser_rejections, 0);
    assert!(parsed.parser_rejection_details.is_empty());
    assert_eq!(parsed.candidates.len(), 1);
    assert_eq!(parsed.instructions.len(), 2);
    assert_eq!(parsed.block_time, Some(1_700_000_000));
    assert_eq!(parsed.events.len(), 1);
    let fact = &parsed.events[0];
    assert_eq!(fact.transaction_index, 3);
    assert_eq!(bs58::decode(&fact.signature).into_vec().unwrap().len(), 64);
    assert_eq!(fact.event.log_index(), 1);
    let JupiterEvent::Operate {
        supply_amount_raw,
        borrow_amount_raw,
        supply_exchange_price_raw,
        borrow_exchange_price_raw,
        ..
    } = &fact.event
    else {
        panic!("wrong event type")
    };
    assert_eq!(supply_amount_raw, "0");
    assert_eq!(borrow_amount_raw, "-10000011");
    assert_eq!(supply_exchange_price_raw, "1052840839848");
    assert_eq!(borrow_exchange_price_raw, "1072842077154");
}

#[test]
fn failed_transaction_is_retained_without_committed_events() {
    let frame = bank_fixture_with_program_logs(
        key(JUPITER_LIQUIDITY_PROGRAM),
        true,
        Some(logs(JUPITER_LIQUIDITY_PROGRAM)),
        1,
        false,
    );
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_scanned, 2);
    assert_eq!(parsed.transactions_selected, 1);
    assert_eq!(parsed.transactions_failed, 1);
    assert_eq!(parsed.transactions_succeeded, 0);
    assert_eq!(parsed.parser_rejections, 0);
    assert!(parsed.parser_rejection_details.is_empty());
    assert!(parsed.events.is_empty());
    assert_eq!(parsed.candidates.len(), 1);
    assert_eq!(parsed.candidates[0].execution_status, "failed");
    assert!(parsed
        .instructions
        .iter()
        .all(|ix| ix.execution_status == "failed_transaction"));
}

#[test]
fn missing_execution_metadata_cannot_create_protocol_facts() {
    let frame = bank_fixture_with_program_logs(
        key(JUPITER_LIQUIDITY_PROGRAM),
        false,
        Some(logs(JUPITER_LIQUIDITY_PROGRAM)),
        1,
        true,
    );
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_selected, 1);
    assert_eq!(parsed.transactions_without_metadata, 1);
    assert_eq!(parsed.transactions_succeeded, 0);
    assert_eq!(parsed.transactions_failed, 0);
    assert_eq!(parsed.candidates.len(), 1);
    assert_eq!(parsed.candidates[0].execution_status, "unknown");
    assert_eq!(parsed.parser_rejections, 0);
    assert!(parsed.parser_rejection_details.is_empty());
    assert!(parsed.events.is_empty());
}

#[test]
fn missing_execution_logs_keep_candidate_and_instructions() {
    let frame =
        bank_fixture_with_program_logs(key(JUPITER_LIQUIDITY_PROGRAM), false, None, 1, false);
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_selected, 1);
    assert_eq!(parsed.transactions_succeeded, 1);
    assert_eq!(parsed.candidates.len(), 1);
    assert_eq!(parsed.instructions.len(), 2);
    assert_eq!(parsed.candidates[0].log_status, "unavailable");
    assert_eq!(parsed.parser_rejections, 1);
    assert_eq!(parsed.parser_rejection_details.len(), 1);
    let rejection = &parsed.parser_rejection_details[0];
    assert_eq!(rejection.transaction_index, 1);
    assert_eq!(
        bs58::decode(&rejection.signature).into_vec().unwrap().len(),
        64
    );
    assert_eq!(rejection.reason, "missing_execution_logs");
    assert!(parsed.events.is_empty());
}

#[test]
fn same_discriminator_from_an_unrelated_program_is_not_selected() {
    let frame = bank_fixture_with_program_logs(key(OTHER), false, Some(logs(OTHER)), 2, false);
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_scanned, 3);
    assert_eq!(parsed.transactions_selected, 0);
    assert_eq!(parsed.transactions_succeeded, 0);
    assert_eq!(parsed.parser_rejections, 0);
    assert!(parsed.events.is_empty());
}

#[test]
fn selected_transaction_cannot_attribute_child_program_events_to_jupiter() {
    let logs = vec![
        invoke(JUPITER_LIQUIDITY_PROGRAM, 1),
        invoke(OTHER, 2),
        operate_log(),
        success(OTHER),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ];
    let frame =
        bank_fixture_with_program_logs(key(JUPITER_LIQUIDITY_PROGRAM), false, Some(logs), 1, false);
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_selected, 1);
    assert_eq!(parsed.transactions_succeeded, 1);
    assert_eq!(parsed.parser_rejections, 0);
    assert!(parsed.events.is_empty());
}

#[test]
fn failed_inner_invocation_is_discarded_inside_a_successful_transaction() {
    let logs = vec![
        invoke(OTHER, 1),
        invoke(JUPITER_LIQUIDITY_PROGRAM, 2),
        operate_log(),
        format!("Program {JUPITER_LIQUIDITY_PROGRAM} failed: custom program error: 0x1"),
        success(OTHER),
    ];
    let frame =
        bank_fixture_with_program_logs(key(JUPITER_LIQUIDITY_PROGRAM), false, Some(logs), 1, false);
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.transactions_selected, 1);
    assert_eq!(parsed.transactions_succeeded, 1);
    assert_eq!(parsed.parser_rejections, 0);
    assert!(parsed.events.is_empty());
}

#[test]
fn bad_target_logs_only_disable_log_enrichment() {
    let mut short = STANDARD.decode(RECORDED_OPERATE).unwrap();
    short.pop();
    for (second_log, reason) in [
        (
            format!("Program data: {}", STANDARD.encode(&short)),
            "invalid Jupiter LogOperate length",
        ),
        (
            "Program data: invalid_base64!".into(),
            "invalid program event base64",
        ),
        ("Log truncated".into(), "transaction logs were truncated"),
        (
            success(OTHER),
            "program completion does not match invocation",
        ),
    ] {
        let logs = vec![
            invoke(JUPITER_LIQUIDITY_PROGRAM, 1),
            operate_log(),
            second_log,
            success(JUPITER_LIQUIDITY_PROGRAM),
        ];
        let frame = bank_fixture_with_program_logs(
            key(JUPITER_LIQUIDITY_PROGRAM),
            false,
            Some(logs),
            1,
            false,
        );
        let parsed = parse_block(&frame).unwrap();
        assert_eq!(parsed.transactions_selected, 1);
        assert_eq!(parsed.transactions_succeeded, 1);
        assert_eq!(parsed.candidates.len(), 1);
        assert_eq!(parsed.instructions.len(), 2);
        assert_eq!(parsed.candidates[0].execution_status, "succeeded");
        assert!(matches!(
            parsed.candidates[0].log_status.as_str(),
            "invalid" | "truncated"
        ));
        assert_eq!(parsed.parser_rejections, 1);
        assert_eq!(parsed.parser_rejection_details.len(), 1);
        let rejection = &parsed.parser_rejection_details[0];
        assert_eq!(rejection.transaction_index, 1);
        assert_eq!(
            bs58::decode(&rejection.signature).into_vec().unwrap().len(),
            64
        );
        assert_eq!(rejection.reason, reason);
        assert!(rejection.reason.len() <= 512);
        assert!(parsed.events.is_empty());
    }
}

#[test]
fn repeated_events_keep_their_occurrence_positions_in_the_original_transaction() {
    let logs = vec![
        invoke(JUPITER_LIQUIDITY_PROGRAM, 1),
        operate_log(),
        operate_log(),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ];
    let frame =
        bank_fixture_with_program_logs(key(JUPITER_LIQUIDITY_PROGRAM), false, Some(logs), 2, false);
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.events.len(), 2);
    assert_eq!(parsed.events[0].transaction_index, 2);
    assert_eq!(parsed.events[1].transaction_index, 2);
    assert_eq!(parsed.events[0].signature, parsed.events[1].signature);
    assert_eq!(parsed.events[0].event.log_index(), 1);
    assert_eq!(parsed.events[1].event.log_index(), 2);
}

#[test]
fn native_exchange_price_event_retains_only_its_emitted_borrow_apr() {
    let mut bytes = vec![190, 194, 69, 204, 30, 86, 181, 163];
    bytes.extend_from_slice(&[9; 32]);
    bytes.extend_from_slice(&1_000_000_000_000u128.to_le_bytes());
    bytes.extend_from_slice(&1_050_000_000_000u128.to_le_bytes());
    bytes.extend_from_slice(&550u16.to_le_bytes());
    bytes.extend_from_slice(&8_000u16.to_le_bytes());
    let logs = vec![
        invoke(JUPITER_LIQUIDITY_PROGRAM, 1),
        format!("Program data: {}", STANDARD.encode(&bytes)),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ];
    let frame =
        bank_fixture_with_program_logs(key(JUPITER_LIQUIDITY_PROGRAM), false, Some(logs), 0, false);
    let parsed = parse_block(&frame).unwrap();
    assert_eq!(parsed.events.len(), 1);
    assert!(matches!(&parsed.events[0].event,
        JupiterEvent::UpdateExchangePrices { borrow_rate_bps: 550, borrow_apr, .. }
            if *borrow_apr == 0.055));
    let json = serde_json::to_value(&parsed.events[0].event).unwrap();
    assert!(json.get("apy").is_none());
    assert!(json.get("supply_apr").is_none());
}
