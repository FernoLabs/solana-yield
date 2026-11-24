use super::*;
use serde_json::json;

const LIQUIDITY: &str = jupiter_ledger::LIQUIDITY_PROGRAM;
const EARN: &str = jupiter_ledger::EARN_PROGRAM;
const KAMINO: &str = kamino_ledger::PROGRAM;
const WRAPPER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";

fn liquidity_log() -> String {
    let mut data = vec![190, 194, 69, 204, 30, 86, 181, 163];
    data.extend([2_u8; 32]);
    data.extend((u64::MAX as u128 + 1).to_le_bytes());
    data.extend(1_000_000_000_000_u128.to_le_bytes());
    data.extend(265_u16.to_le_bytes());
    data.extend(3_000_u16.to_le_bytes());
    format!("Program data: {}", STANDARD.encode(data))
}

fn earn_log() -> String {
    let mut data = vec![222, 11, 113, 60, 147, 15, 68, 217];
    data.extend(1_100_000_000_000_u64.to_le_bytes());
    data.extend(1_050_000_000_000_u64.to_le_bytes());
    format!("Program data: {}", STANDARD.encode(data))
}

fn tx(logs: Value) -> Value {
    json!({
        "transaction": { "message": {"accountKeys":[], "instructions":[]} },
        "meta": { "err":null, "logMessages":logs }
    })
}

fn update_rate_instruction() -> Value {
    json!({
        "programIdIndex":0, "accounts":[1,2,3,4,5],
        "data":bs58::encode([24,225,53,189,72,212,225,178]).into_string()
    })
}

#[test]
fn retains_exact_liquidity_value_only_from_a_successful_emitter() {
    let value = tx(json!([
        format!("Program {WRAPPER} invoke [1]"),
        liquidity_log(),
        format!("Program {LIQUIDITY} invoke [2]"),
        liquidity_log(),
        format!("Program {LIQUIDITY} success"),
        format!("Program {WRAPPER} success"),
    ]));
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.log_status, LogStatus::Complete);
    assert_eq!(facts.measurements.len(), 1);
    let output = serde_json::to_value(&facts.measurements[0]).unwrap();
    assert_eq!(output["log_index"], 3);
    assert_eq!(
        output["measurement"]["supply_exchange_price"],
        "18446744073709551616"
    );
    assert_eq!(output["measurement"]["borrow_rate_bps"], 265);
}

#[test]
fn drops_successful_children_of_a_failed_call() {
    let child = "11111111111111111111111111111111";
    let value = tx(json!([
        format!("Program {WRAPPER} invoke [1]"),
        format!("Program {child} invoke [2]"),
        format!("Program {LIQUIDITY} invoke [3]"),
        liquidity_log(),
        format!("Program {LIQUIDITY} success"),
        format!("Program {child} failed: error"),
        format!("Program {WRAPPER} success"),
    ]));
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.log_status, LogStatus::Complete);
    assert!(facts.measurements.is_empty());
}

#[test]
fn preserves_absent_truncated_invalid_and_failed_transactions() {
    let mut value = tx(Value::Null);
    assert_eq!(
        parse_transaction(&value).unwrap().log_status,
        LogStatus::Unavailable
    );
    value["meta"]["logMessages"] = json!([
        format!("Program {LIQUIDITY} invoke [1]"),
        liquidity_log(),
        format!("Program {LIQUIDITY} success"),
        "Log truncated",
    ]);
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.log_status, LogStatus::Truncated);
    assert!(facts.measurements.is_empty());
    value["meta"]["logMessages"] = json!([
        format!("Program {LIQUIDITY} invoke [1]"),
        liquidity_log(),
        format!("Program {WRAPPER} success"),
    ]);
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.log_status, LogStatus::Invalid);
    assert!(facts.measurements.is_empty());
    value["meta"]["err"] = json!({"InstructionError":[0,"InvalidArgument"]});
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.execution_status, ExecutionStatus::Failed);
    assert!(facts.measurements.is_empty());
}

#[test]
fn binds_earn_to_one_top_level_market_and_preserves_ambiguous_events() {
    let mut value = tx(json!([
        format!("Program {EARN} invoke [1]"),
        earn_log(),
        format!("Program {EARN} success"),
    ]));
    let keys: Vec<_> = (2..=6)
        .map(|byte| bs58::encode([byte; 32]).into_string())
        .collect();
    value["transaction"]["message"]["accountKeys"] = json!([EARN, keys[0], keys[1]]);
    value["meta"]["loadedAddresses"] = json!({"writable":[keys[2]],"readonly":[keys[3],keys[4]]});
    value["transaction"]["message"]["instructions"] = json!([update_rate_instruction()]);
    let found = parse_transaction(&value).unwrap();
    assert_eq!(found.measurements.len(), 1);
    assert_eq!(found.measurements[0].outer_instruction_index, Some(0));
    assert_eq!(found.measurements[0].earn_market.unwrap().pool, [2; 32]);

    value["transaction"]["message"]["instructions"] =
        json!([update_rate_instruction(), update_rate_instruction()]);
    let found = parse_transaction(&value).unwrap();
    assert_eq!(found.measurements.len(), 1);
    assert!(found.measurements[0].earn_market.is_none());

    value["transaction"]["message"]["instructions"] = json!([update_rate_instruction()]);
    value["meta"]["logMessages"] = json!([
        format!("Program {WRAPPER} invoke [1]"),
        format!("Program {EARN} invoke [2]"),
        earn_log(),
        format!("Program {EARN} success"),
        format!("Program {WRAPPER} success"),
    ]);
    let found = parse_transaction(&value).unwrap();
    assert_eq!(found.measurements.len(), 1);
    assert!(found.measurements[0].earn_market.is_none());
}

#[test]
fn binds_kamino_return_to_the_final_getter_and_its_reserve() {
    let mut value = tx(Value::Null);
    let reserve = bs58::encode([5_u8; 32]).into_string();
    let mut returned = (1_u128 << 79).to_le_bytes().to_vec();
    returned.push(6);
    value["transaction"]["message"] = json!({
        "accountKeys":[KAMINO,reserve],
        "instructions":[
            {"programIdIndex":0,"accounts":[1],"data":"ZZ"},
            {"programIdIndex":0,"accounts":[1],"data":bs58::encode([32,253,220,13,19,165,131,188]).into_string()}
        ]
    });
    value["meta"]["innerInstructions"] = json!([{"index":0,"instructions":[{}]}]);
    value["meta"]["returnData"] =
        json!({"programId":KAMINO,"data":[STANDARD.encode(returned),"base64"]});
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.log_status, LogStatus::Unavailable);
    assert_eq!(facts.kamino_return_status, ReturnStatus::Decoded);
    assert_eq!(facts.measurements[0].outer_instruction_index, Some(1));
    assert!(
        matches!(facts.measurements[0].measurement, LedgerMeasurement::KaminoReserveExchangePrice { reserve, liquidity_mint_decimals: 6, .. } if reserve == [5; 32])
    );

    value["meta"]["innerInstructions"] = json!([{"index":1,"instructions":[{}]}]);
    let facts = parse_transaction(&value).unwrap();
    assert_eq!(facts.kamino_return_status, ReturnStatus::Unavailable);
    assert!(facts.measurements.is_empty());
}
