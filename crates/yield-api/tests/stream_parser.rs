use base64::{engine::general_purpose::STANDARD, Engine};
use yield_indexer::stream_parser::{parse_jupiter_logs, JupiterEvent, JUPITER_LIQUIDITY_PROGRAM};

const OTHER: &str = "jupr81YtYssSyPt8jbnGuiWon5f6x9TcDEFxYe3Bdzi";

fn begin(program: &str, depth: usize) -> String {
    format!("Program {program} invoke [{depth}]")
}
fn success(program: &str) -> String {
    format!("Program {program} success")
}
fn failed(program: &str) -> String {
    format!("Program {program} failed: custom program error: 0x1")
}
fn data(bytes: &[u8]) -> String {
    format!("Program data: {}", STANDARD.encode(bytes))
}
fn rate() -> Vec<u8> {
    let mut bytes = vec![190, 194, 69, 204, 30, 86, 181, 163];
    bytes.extend_from_slice(&[9; 32]);
    bytes.extend_from_slice(&u128::MAX.to_le_bytes());
    bytes.extend_from_slice(&1_072_842_077_154u128.to_le_bytes());
    bytes.extend_from_slice(&547u16.to_le_bytes());
    bytes.extend_from_slice(&8_500u16.to_le_bytes());
    bytes
}
fn operate() -> Vec<u8> {
    let mut bytes = vec![180, 8, 81, 71, 19, 132, 173, 8];
    bytes.extend_from_slice(&[8; 32]);
    bytes.extend_from_slice(&[9; 32]);
    bytes.extend_from_slice(&i128::MIN.to_le_bytes());
    bytes.extend_from_slice(&i128::MAX.to_le_bytes());
    bytes.extend_from_slice(&[0; 32]);
    bytes.extend_from_slice(&[7; 32]);
    bytes.extend_from_slice(&1_000_072_659_663u64.to_le_bytes());
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    bytes
}
fn direct(bytes: &[u8]) -> Vec<String> {
    vec![
        begin(JUPITER_LIQUIDITY_PROGRAM, 1),
        data(bytes),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ]
}

#[test]
fn decodes_protocol_rate_without_losing_wide_values_or_inventing_apy() {
    let events = parse_jupiter_logs(&direct(&rate())).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].log_index(), 1);
    assert_eq!(events[0].token(), bs58::encode([9; 32]).into_string());
    let JupiterEvent::UpdateExchangePrices {
        supply_exchange_price_raw,
        borrow_exchange_price_raw,
        borrow_rate_bps,
        borrow_apr,
        utilization_bps,
        ..
    } = &events[0]
    else {
        panic!("wrong event type")
    };
    assert_eq!(supply_exchange_price_raw, &u128::MAX.to_string());
    assert_eq!(borrow_exchange_price_raw, "1072842077154");
    assert_eq!(*borrow_rate_bps, 547);
    assert_eq!(*borrow_apr, 0.0547);
    assert_eq!(*utilization_bps, 8_500);
    let json = serde_json::to_value(&events[0]).unwrap();
    assert!(json.get("apy").is_none());
    assert_eq!(
        serde_json::from_value::<JupiterEvent>(json).unwrap(),
        events[0]
    );
}

#[test]
fn operate_keeps_signed_raw_units_and_has_no_reported_rate() {
    let events = parse_jupiter_logs(&direct(&operate())).unwrap();
    let JupiterEvent::Operate {
        supply_amount_raw,
        borrow_amount_raw,
        withdraw_to,
        supply_exchange_price_raw,
        borrow_exchange_price_raw,
        ..
    } = &events[0]
    else {
        panic!("wrong event type")
    };
    assert_eq!(supply_amount_raw, &i128::MIN.to_string());
    assert_eq!(borrow_amount_raw, &i128::MAX.to_string());
    assert_eq!(withdraw_to, "11111111111111111111111111111111");
    assert_eq!(supply_exchange_price_raw, "1000072659663");
    assert_eq!(borrow_exchange_price_raw, &u64::MAX.to_string());
    let json = serde_json::to_value(&events[0]).unwrap();
    assert!(json.get("borrow_apr").is_none());
    assert!(json.get("apy").is_none());
}

#[test]
fn ignores_same_discriminator_in_other_program_and_child_program() {
    // The vault program uses the same Anchor event name and discriminator.
    assert!(
        parse_jupiter_logs(&[begin(OTHER, 1), data(&operate()), success(OTHER),])
            .unwrap()
            .is_empty()
    );
    let logs = vec![
        begin(JUPITER_LIQUIDITY_PROGRAM, 1),
        begin(OTHER, 2),
        data(&rate()),
        success(OTHER),
        data(&operate()),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ];
    let events = parse_jupiter_logs(&logs).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        JupiterEvent::Operate { log_index: 4, .. }
    ));
}

#[test]
fn nested_invocations_preserve_log_order_and_duplicate_occurrences() {
    let logs = vec![
        begin(OTHER, 1),
        begin(JUPITER_LIQUIDITY_PROGRAM, 2),
        data(&rate()),
        success(JUPITER_LIQUIDITY_PROGRAM),
        begin(JUPITER_LIQUIDITY_PROGRAM, 2),
        data(&rate()),
        success(JUPITER_LIQUIDITY_PROGRAM),
        success(OTHER),
    ];
    let events = parse_jupiter_logs(&logs).unwrap();
    assert_eq!(
        events
            .iter()
            .map(JupiterEvent::log_index)
            .collect::<Vec<_>>(),
        [2, 5]
    );
}

#[test]
fn failed_cpi_discards_events_but_keeps_later_successful_cpi() {
    let logs = vec![
        begin(OTHER, 1),
        begin(JUPITER_LIQUIDITY_PROGRAM, 2),
        data(&rate()),
        failed(JUPITER_LIQUIDITY_PROGRAM),
        begin(JUPITER_LIQUIDITY_PROGRAM, 2),
        data(&operate()),
        success(JUPITER_LIQUIDITY_PROGRAM),
        success(OTHER),
    ];
    let events = parse_jupiter_logs(&logs).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].log_index(), 5);
}

#[test]
fn failed_parent_discards_successful_child_events() {
    let logs = vec![
        begin(OTHER, 1),
        begin(JUPITER_LIQUIDITY_PROGRAM, 2),
        data(&rate()),
        success(JUPITER_LIQUIDITY_PROGRAM),
        failed(OTHER),
    ];
    assert!(parse_jupiter_logs(&logs).unwrap().is_empty());
}

#[test]
fn rejects_truncated_known_events_and_extra_bytes() {
    for bytes in [rate(), operate()] {
        for length in [1, 7, 8, bytes.len() - 1] {
            assert!(parse_jupiter_logs(&direct(&bytes[..length])).is_err());
        }
        let mut extended = bytes;
        extended.push(0);
        assert!(parse_jupiter_logs(&direct(&extended)).is_err());
    }
}

#[test]
fn rejects_invalid_base64_missing_frames_depths_completions_and_truncation() {
    for logs in [
        vec![data(&rate())],
        vec![begin(JUPITER_LIQUIDITY_PROGRAM, 1), data(&rate())],
        vec![
            begin(JUPITER_LIQUIDITY_PROGRAM, 2),
            data(&rate()),
            success(JUPITER_LIQUIDITY_PROGRAM),
        ],
        vec![
            begin(JUPITER_LIQUIDITY_PROGRAM, 1),
            data(&rate()),
            success(OTHER),
        ],
        vec![
            begin(JUPITER_LIQUIDITY_PROGRAM, 1),
            "Program data: !bad!".into(),
            success(JUPITER_LIQUIDITY_PROGRAM),
        ],
        vec![
            begin(JUPITER_LIQUIDITY_PROGRAM, 1),
            data(&rate()),
            "Log truncated".into(),
        ],
        vec![success(JUPITER_LIQUIDITY_PROGRAM)],
    ] {
        assert!(parse_jupiter_logs(&logs).is_err(), "accepted {logs:?}");
    }
}

#[test]
fn user_log_cannot_open_or_close_an_invocation_frame() {
    let logs = vec![
        begin(JUPITER_LIQUIDITY_PROGRAM, 1),
        format!("Program log: Program {OTHER} success"),
        format!("Program log: Program {OTHER} invoke [2]"),
        data(&rate()),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ];
    assert_eq!(parse_jupiter_logs(&logs).unwrap().len(), 1);
    assert!(parse_jupiter_logs(&[
        format!("Program log: Program {JUPITER_LIQUIDITY_PROGRAM} invoke [1]"),
        data(&rate()),
    ])
    .is_err());
}

#[test]
fn unknown_events_and_empty_logs_do_not_produce_values() {
    assert!(parse_jupiter_logs(&[]).unwrap().is_empty());
    assert!(parse_jupiter_logs(&direct(&[1, 2, 3])).unwrap().is_empty());
    assert!(parse_jupiter_logs(&direct(&[1; 64])).unwrap().is_empty());
}

#[test]
fn bounds_transaction_lines_payload_and_event_count() {
    assert!(parse_jupiter_logs(&vec![String::new(); 4_097]).is_err());
    assert!(parse_jupiter_logs(&["x".repeat(16_385)]).is_err());
    assert!(parse_jupiter_logs(&vec!["x".repeat(16_384); 65]).is_err());
    assert!(parse_jupiter_logs(&direct(&vec![1; 4_097])).is_err());
    let mut logs = vec![begin(JUPITER_LIQUIDITY_PROGRAM, 1)];
    logs.extend((0..513).map(|_| data(&rate())));
    logs.push(success(JUPITER_LIQUIDITY_PROGRAM));
    assert!(parse_jupiter_logs(&logs).is_err());
}

#[test]
fn recorded_local_fork_liquidity_events_match_native_exchange_prices() {
    // Public recorded local-fork event from carry-switch-receipts-fork-2026-09-14.json.
    // It proves decoding, not live mainnet coverage or current rates.
    let encoded = "tAhRRxOErQhEOX5oO2595aSDxpiEgZ0cyWoqkwBD2Y83QoyhbWSFOxwu5n8STfbt8qnSv146Uzx3AbWPjaqVchCh5bBhjNK+AAAAAAAAAAAAAAAAAAAAAHVpZ/////////////////9l9d/9fISvdail973OoWIuKzq/h/xTqHG5CFYaOosb3mX13/18hK91qKX3vc6hYi4rOr+H/FOocbkIVho6ixveqEo0IvUAAADi817K+QAAAA==";
    let logs = vec![
        begin(JUPITER_LIQUIDITY_PROGRAM, 1),
        format!("Program data: {encoded}"),
        success(JUPITER_LIQUIDITY_PROGRAM),
    ];
    let events = parse_jupiter_logs(&logs).unwrap();
    let JupiterEvent::Operate {
        supply_amount_raw,
        borrow_amount_raw,
        supply_exchange_price_raw,
        borrow_exchange_price_raw,
        ..
    } = &events[0]
    else {
        panic!("wrong event type")
    };
    assert_eq!(supply_amount_raw, "0");
    assert_eq!(borrow_amount_raw, "-10000011");
    assert_eq!(supply_exchange_price_raw, "1052840839848");
    assert_eq!(borrow_exchange_price_raw, "1072842077154");
}
