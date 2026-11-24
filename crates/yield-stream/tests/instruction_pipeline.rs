//! Instruction admission and decoding do not depend on execution log coverage.
//! All inputs are local synthetic transactions; no network or RPC is used.
use blockzilla_decoder::DecodedBlock;
use serde_json::{json, Value};
use yield_indexer::stream_parser::JUPITER_LIQUIDITY_PROGRAM;
use yield_stream::pipeline::{parse_candidate, parse_projected_block, PARSER_VERSION};

const OTHER: &str = "jupr81YtYssSyPt8jbnGuiWon5f6x9TcDEFxYe3Bdzi";

fn key(byte: u8) -> String {
    bs58::encode([byte; 32]).into_string()
}
fn signature() -> String {
    bs58::encode([8; 64]).into_string()
}
fn exchange_update() -> String {
    let mut bytes = vec![239, 244, 10, 248, 116, 25, 53, 150];
    bytes.extend_from_slice(&[9; 32]);
    bs58::encode(bytes).into_string()
}
fn compiled(program: usize, accounts: &[usize], data: &str) -> Value {
    json!({ "programIdIndex": program, "accounts": accounts, "data": data })
}
fn transaction() -> Value {
    json!({
        "transaction": {
            "signatures": [signature()],
            "message": {
                "header": { "numRequiredSignatures": 1, "numReadonlySignedAccounts": 0,
                    "numReadonlyUnsignedAccounts": 1 },
                "accountKeys": [OTHER, key(1), key(2), JUPITER_LIQUIDITY_PROGRAM],
                "recentBlockhash": key(7),
                "instructions": [compiled(3, &[1, 2], &exchange_update())],
                "addressTableLookups": [],
            }
        },
        "meta": {
            "err": null, "status": { "Ok": null }, "fee": 5000,
            "preBalances": [10000, 0, 0, 0], "postBalances": [5000, 0, 0, 0],
            "innerInstructions": [], "logMessages": [],
            "preTokenBalances": [], "postTokenBalances": [], "rewards": [],
            "loadedAddresses": { "writable": [], "readonly": [] },
            "returnData": null, "computeUnitsConsumed": 1000,
        },
        "version": 0,
    })
}

#[test]
fn primary_instruction_survives_missing_truncated_and_invalid_logs() {
    for (logs, expected_status) in [
        (None, "unavailable"),
        (Some(Value::Null), "unavailable"),
        (Some(json!(["Log truncated"])), "truncated"),
        (Some(json!(["Program log: Log truncated"])), "truncated"),
        (Some(json!([42])), "invalid"),
        (Some(json!([format!("Program {OTHER} success")])), "invalid"),
    ] {
        let mut input = transaction();
        match logs {
            Some(logs) => input["meta"]["logMessages"] = logs,
            None => {
                input["meta"].as_object_mut().unwrap().remove("logMessages");
            }
        }
        let (candidate, instructions, events, enrichment_failure) =
            parse_candidate(input, 41).unwrap();
        assert_eq!(candidate.transaction_index, 41);
        assert_eq!(candidate.execution_status, "succeeded");
        assert_eq!(candidate.log_status, expected_status);
        assert_eq!(candidate.parser_version, PARSER_VERSION);
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].name, "update_exchange_price");
        assert_eq!(instructions[0].decode_status, "decoded");
        assert_eq!(instructions[0].execution_status, "committed_top_level");
        assert!(events.is_empty());
        assert!(enrichment_failure.is_some());
        assert!(candidate.transaction.pointer("/meta/logMessages").is_none());
    }
}

#[test]
fn a_program_key_or_log_name_alone_does_not_become_a_jupiter_call() {
    let mut input = transaction();
    input["transaction"]["message"]["instructions"] = json!([
        compiled(0, &[1, 2], &exchange_update()),
        { "programId": format!("{JUPITER_LIQUIDITY_PROGRAM}x"),
          "accounts": [key(1), key(2)], "data": exchange_update() },
    ]);
    input["meta"]["logMessages"] = json!([format!("Program log: {JUPITER_LIQUIDITY_PROGRAM}")]);
    let (candidate, instructions, events, _) = parse_candidate(input, 4).unwrap();
    assert_eq!(candidate.signature, signature());
    assert!(instructions.is_empty());
    assert!(events.is_empty());
}

#[test]
fn resolves_loaded_addresses_for_outer_and_inner_calls_and_keeps_original_positions() {
    let mut input = transaction();
    input["transaction"]["message"]["accountKeys"] = json!([OTHER, key(2)]);
    input["meta"]["loadedAddresses"] = json!({
        "writable": [key(1)], "readonly": [JUPITER_LIQUIDITY_PROGRAM],
    });
    input["transaction"]["message"]["instructions"] = json!([
        compiled(0, &[], ""),
        compiled(3, &[2, 1], &exchange_update()),
    ]);
    let mut inner = compiled(3, &[2, 1], &exchange_update());
    inner["stackHeight"] = json!(3);
    input["meta"]["innerInstructions"] = json!([
        { "index": 0, "instructions": [compiled(0, &[], ""), inner] },
    ]);
    let (candidate, instructions, events, _) = parse_candidate(input, 72).unwrap();
    assert_eq!(candidate.transaction_index, 72);
    assert_eq!(instructions.len(), 2);
    let cpi = &instructions[0];
    assert_eq!(cpi.transaction_index, 72);
    assert_eq!(cpi.outer_instruction_index, 0);
    assert_eq!(cpi.inner_instruction_index, Some(1));
    assert_eq!(cpi.stack_height, Some(3));
    assert_eq!(cpi.execution_status, "attempted_cpi_unproven");
    let outer = &instructions[1];
    assert_eq!(outer.outer_instruction_index, 1);
    assert_eq!(outer.inner_instruction_index, None);
    assert_eq!(outer.execution_status, "committed_top_level");
    for instruction in &instructions {
        assert_eq!(instruction.program, JUPITER_LIQUIDITY_PROGRAM);
        assert_eq!(instruction.accounts, [key(1), key(2)]);
        assert_eq!(
            instruction.arguments["accounts"][0]["name"],
            "token_reserve"
        );
        assert_eq!(instruction.arguments["accounts"][0]["address"], key(1));
        assert_eq!(instruction.arguments["accounts"][1]["name"], "rate_model");
        assert_eq!(instruction.arguments["arguments"]["_mint"], key(9));
        assert_eq!(instruction.decode_status, "decoded");
    }
    assert!(events.is_empty());
}

#[test]
fn parsed_account_keys_are_already_resolved_and_not_extended_twice() {
    let mut input = transaction();
    input["transaction"]["message"]["accountKeys"] = json!([
        { "pubkey": OTHER, "signer": true, "writable": true, "source": "transaction" },
        { "pubkey": key(1), "signer": false, "writable": true, "source": "lookupTable" },
        { "pubkey": key(2), "signer": false, "writable": false, "source": "transaction" },
        { "pubkey": JUPITER_LIQUIDITY_PROGRAM, "signer": false, "writable": false, "source": "lookupTable" },
    ]);
    input["meta"]["loadedAddresses"] = json!({
        "writable": [key(1)], "readonly": [JUPITER_LIQUIDITY_PROGRAM],
    });
    // An implementation that appends loaded keys again would invent a second
    // Jupiter instruction at program index 5. That index is not in the message.
    input["transaction"]["message"]["instructions"] = json!([
        compiled(3, &[1, 2], &exchange_update()),
        compiled(5, &[1, 2], &exchange_update()),
    ]);
    let (_, instructions, _, _) = parse_candidate(input, 0).unwrap();
    assert_eq!(instructions.len(), 1);
    assert_eq!(instructions[0].outer_instruction_index, 0);
    assert_eq!(instructions[0].accounts, [key(1), key(2)]);
    assert_eq!(instructions[0].decode_status, "decoded");
}

#[test]
fn accepts_partially_decoded_ui_instructions_with_direct_account_addresses() {
    let mut input = transaction();
    input["transaction"]["message"]["instructions"] = json!([
        { "programId": JUPITER_LIQUIDITY_PROGRAM, "accounts": [key(1), key(2)],
          "data": exchange_update(), "stackHeight": 1 },
    ]);
    input["meta"]["innerInstructions"] = json!([
        { "index": 0, "instructions": [
            { "programId": JUPITER_LIQUIDITY_PROGRAM, "accounts": [key(1), key(2)],
              "data": exchange_update(), "stackHeight": 2 },
        ] },
    ]);
    let (_, instructions, _, _) = parse_candidate(input, 0).unwrap();
    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions[0].decode_status, "decoded");
    assert_eq!(instructions[0].stack_height, Some(1));
    assert_eq!(instructions[1].decode_status, "decoded");
    assert_eq!(instructions[1].stack_height, Some(2));
    assert_eq!(instructions[1].execution_status, "attempted_cpi_unproven");
}

#[test]
fn failed_transactions_remain_candidates_with_unapplied_instruction_requests() {
    let mut input = transaction();
    input["meta"]["err"] = json!({ "InstructionError": [0, { "Custom": 1 }] });
    input["meta"]["innerInstructions"] = json!([
        { "index": 0, "instructions": [compiled(3, &[1, 2], &exchange_update())] },
    ]);
    let (candidate, instructions, events, failure) = parse_candidate(input, 19).unwrap();
    assert_eq!(candidate.execution_status, "failed");
    assert_eq!(
        candidate.transaction["meta"]["err"]["InstructionError"][0],
        0
    );
    assert_eq!(instructions.len(), 2);
    for instruction in instructions {
        assert_eq!(instruction.execution_status, "failed_transaction");
        assert_eq!(instruction.decode_status, "decoded");
        assert_eq!(instruction.arguments["requested_only"], true);
    }
    assert!(events.is_empty());
    assert!(failure.is_none());
}

#[test]
fn missing_metadata_keeps_static_instruction_requests_with_unknown_execution() {
    for remove in [false, true] {
        let mut input = transaction();
        if remove {
            input.as_object_mut().unwrap().remove("meta");
        } else {
            input["meta"] = Value::Null;
        }
        let (candidate, instructions, events, failure) = parse_candidate(input, 9).unwrap();
        assert_eq!(candidate.execution_status, "unknown");
        assert_eq!(candidate.log_status, "unavailable");
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].execution_status, "unknown_transaction");
        assert_eq!(instructions[0].decode_status, "decoded");
        assert!(events.is_empty());
        assert!(failure.is_none());
    }
}

#[test]
fn unsupported_and_invalid_instruction_data_are_retained_without_candidate_loss() {
    let valid = bs58::decode(exchange_update()).into_vec().unwrap();
    for (data, status) in [
        (bs58::encode([255; 8]).into_string(), "unsupported"),
        (bs58::encode(&valid[..39]).into_string(), "invalid"),
        ("invalid_base58!".to_owned(), "invalid"),
    ] {
        let mut input = transaction();
        input["transaction"]["message"]["instructions"][0]["data"] = json!(data);
        let (candidate, instructions, _, _) = parse_candidate(input, 3).unwrap();
        assert_eq!(candidate.signature, signature());
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].decode_status, status);
        assert_eq!(instructions[0].data_base58, data);
        assert!(instructions[0].decode_reason.is_some());
        assert_eq!(
            candidate.transaction["transaction"]["message"]["instructions"][0]["data"],
            data
        );
    }
}

#[test]
fn missing_ui_raw_fields_are_recorded_as_invalid_without_dropping_the_candidate() {
    let mut input = transaction();
    input["transaction"]["message"]["instructions"] = json!([
        { "programId": JUPITER_LIQUIDITY_PROGRAM, "parsed": { "type": "operate" } },
    ]);
    let (candidate, instructions, _, _) = parse_candidate(input, 1).unwrap();
    assert_eq!(candidate.execution_status, "succeeded");
    assert_eq!(instructions.len(), 1);
    assert_eq!(instructions[0].decode_status, "invalid");
    assert_eq!(
        candidate.transaction["transaction"]["message"]["instructions"][0]["parsed"]["type"],
        "operate"
    );
}

#[test]
fn operate_argument_sentinels_are_requests_and_do_not_become_transferred_amounts() {
    let mut input = transaction();
    let mut keys = vec![JUPITER_LIQUIDITY_PROGRAM.to_owned()];
    keys.extend((1..=13).map(key));
    let mut data = vec![217, 106, 208, 99, 116, 151, 42, 135];
    data.extend_from_slice(&i128::MIN.to_le_bytes());
    data.extend_from_slice(&i128::MAX.to_le_bytes());
    data.extend_from_slice(&[0; 32]);
    data.extend_from_slice(&[7; 32]);
    data.push(1);
    input["transaction"]["message"]["accountKeys"] = json!(keys);
    input["transaction"]["message"]["instructions"] = json!([compiled(
        0,
        &(1..=13).collect::<Vec<_>>(),
        &bs58::encode(data).into_string()
    ),]);
    let (_, instructions, _, _) = parse_candidate(input, 16).unwrap();
    let fact = &instructions[0];
    assert_eq!(fact.name, "operate");
    assert_eq!(fact.arguments["requested_only"], true);
    assert_eq!(
        fact.arguments["arguments"]["supply_amount"],
        i128::MIN.to_string()
    );
    assert_eq!(
        fact.arguments["arguments"]["borrow_amount"],
        i128::MAX.to_string()
    );
    assert!(fact.arguments["arguments"]
        .get("actual_supply_amount")
        .is_none());
    assert!(fact.arguments["arguments"]
        .get("supply_exchange_price")
        .is_none());
}

#[test]
fn cpi_attempts_are_never_marked_committed_by_transaction_success_or_log_claims() {
    for logs in [
        json!([]),
        json!(["Log truncated"]),
        json!([
            format!("Program {OTHER} invoke [1]"),
            format!("Program {JUPITER_LIQUIDITY_PROGRAM} invoke [2]"),
            format!("Program {JUPITER_LIQUIDITY_PROGRAM} success"),
            format!("Program {OTHER} success"),
        ]),
    ] {
        let mut input = transaction();
        input["transaction"]["message"]["instructions"] = json!([compiled(0, &[], "")]);
        input["meta"]["innerInstructions"] = json!([
            { "index": 0, "instructions": [compiled(3, &[1, 2], &exchange_update())] },
        ]);
        input["meta"]["logMessages"] = logs;
        let (candidate, instructions, _, _) = parse_candidate(input, 2).unwrap();
        assert_eq!(candidate.execution_status, "succeeded");
        assert_eq!(instructions.len(), 1);
        assert_eq!(instructions[0].execution_status, "attempted_cpi_unproven");
        assert_eq!(instructions[0].inner_instruction_index, Some(0));
        assert_eq!(instructions[0].decode_status, "decoded");
    }
}

#[test]
fn candidate_retains_instruction_and_balance_evidence_while_removing_log_text() {
    let mut input = transaction();
    let inner = json!([{ "index": 0, "instructions": [compiled(3, &[1, 2], &exchange_update())] }]);
    let pre = json!([{ "accountIndex": 1, "mint": key(9), "owner": key(8),
        "uiTokenAmount": { "amount": "123", "decimals": 6, "uiAmount": 0.000123, "uiAmountString": "0.000123" } }]);
    let post = json!([{ "accountIndex": 1, "mint": key(9), "owner": key(8),
        "uiTokenAmount": { "amount": "122", "decimals": 6, "uiAmount": 0.000122, "uiAmountString": "0.000122" } }]);
    input["meta"]["innerInstructions"] = inner.clone();
    input["meta"]["preTokenBalances"] = pre.clone();
    input["meta"]["postTokenBalances"] = post.clone();
    input["meta"]["logMessages"] = json!(["unnecessary log text"]);
    let (candidate, instructions, _, _) = parse_candidate(input, 11).unwrap();
    assert_eq!(instructions.len(), 2);
    assert!(candidate.transaction.pointer("/meta/logMessages").is_none());
    assert_eq!(candidate.transaction["meta"]["innerInstructions"], inner);
    assert_eq!(candidate.transaction["meta"]["preTokenBalances"], pre);
    assert_eq!(candidate.transaction["meta"]["postTokenBalances"], post);
    assert!(candidate.transaction["transaction"]["message"]["instructions"].is_array());
    assert_eq!(
        candidate.transaction["meta"]["loadedAddresses"],
        json!({ "writable": [], "readonly": [] })
    );
}

#[test]
fn projected_blocks_keep_source_positions_and_count_all_execution_states() {
    let success = transaction();
    let mut failed = transaction();
    failed["meta"]["err"] = json!({ "InstructionError": [0, { "Custom": 1 }] });
    failed["meta"]["status"] = json!({ "Err": { "InstructionError": [0, { "Custom": 1 }] } });
    let mut unknown = transaction();
    unknown["meta"] = Value::Null;
    let block = serde_json::from_value(json!({
        "previousBlockhash": key(6), "blockhash": key(7), "parentSlot": 99,
        "transactions": [success, failed, unknown], "rewards": [],
        "blockTime": 1_700_000_000, "blockHeight": 90,
    }))
    .unwrap();
    let parsed = parse_projected_block(DecodedBlock {
        slot: 100,
        transaction_indices: vec![7, 33, 54],
        total_transactions: 60,
        block,
    })
    .unwrap();
    assert_eq!(parsed.transactions_scanned, 60);
    assert_eq!(parsed.transactions_selected, 3);
    assert_eq!(parsed.transactions_succeeded, 1);
    assert_eq!(parsed.transactions_failed, 1);
    assert_eq!(parsed.transactions_without_metadata, 1);
    assert_eq!(
        parsed
            .candidates
            .iter()
            .map(|candidate| candidate.transaction_index)
            .collect::<Vec<_>>(),
        [7, 33, 54]
    );
    assert_eq!(
        parsed
            .instructions
            .iter()
            .map(|instruction| instruction.transaction_index)
            .collect::<Vec<_>>(),
        [7, 33, 54]
    );
    assert_eq!(
        parsed.instructions[0].execution_status,
        "committed_top_level"
    );
    assert_eq!(
        parsed.instructions[1].execution_status,
        "failed_transaction"
    );
    assert_eq!(
        parsed.instructions[2].execution_status,
        "unknown_transaction"
    );
    assert!(parsed.events.is_empty());
}
