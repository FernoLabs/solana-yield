use serde_json::json;
use yield_indexer::instruction_parser::{parse_jupiter_instruction, JupiterInstruction};
use yield_indexer::stream_parser::JUPITER_LIQUIDITY_PROGRAM;

fn accounts(count: usize) -> Vec<String> {
    (1..=count)
        .map(|index| bs58::encode([index as u8; 32]).into_string())
        .collect()
}

fn operate() -> Vec<u8> {
    let mut data = vec![217, 106, 208, 99, 116, 151, 42, 135];
    data.extend_from_slice(&i128::MIN.to_le_bytes());
    data.extend_from_slice(&i128::MAX.to_le_bytes());
    data.extend_from_slice(&[0; 32]);
    data.extend_from_slice(&[7; 32]);
    data.push(1);
    data
}

#[test]
fn operates_without_logs_and_preserves_requested_amount_sentinels() {
    let instruction = parse_jupiter_instruction(&operate(), &accounts(13))
        .unwrap()
        .unwrap();
    assert_eq!(instruction.name, "operate");
    assert!(instruction.requested_only);
    assert_eq!(
        instruction.arguments["supply_amount"],
        i128::MIN.to_string()
    );
    assert_eq!(
        instruction.arguments["borrow_amount"],
        i128::MAX.to_string()
    );
    assert_eq!(instruction.arguments["transfer_type"], "DIRECT");
    assert_eq!(
        instruction.arguments["withdraw_to"],
        "11111111111111111111111111111111"
    );
    assert_eq!(
        instruction.arguments["borrow_to"],
        bs58::encode([7; 32]).into_string()
    );
    assert_eq!(instruction.accounts[0].name, "protocol");
    assert_eq!(instruction.accounts[2].name, "token_reserve");
    assert_eq!(instruction.accounts[3].name, "mint");
    assert_eq!(instruction.accounts[7].name, "rate_model");
    assert_eq!(instruction.accounts[12].name, "token_program");
    assert!(instruction.arguments.get("borrow_apr").is_none());
    assert!(instruction.arguments.get("apy").is_none());
    assert!(instruction.arguments.get("supply_exchange_price").is_none());
    let serialized = serde_json::to_value(&instruction).unwrap();
    assert_eq!(
        serde_json::from_value::<JupiterInstruction>(serialized).unwrap(),
        instruction
    );
}

#[test]
fn retains_optional_account_positions_and_remaining_keys() {
    let mut keys = accounts(15);
    for index in [5, 6, 8, 9, 10, 11] {
        keys[index] = JUPITER_LIQUIDITY_PROGRAM.to_owned();
    }
    let instruction = parse_jupiter_instruction(&operate(), &keys)
        .unwrap()
        .unwrap();
    assert_eq!(instruction.accounts.len(), 13);
    for index in [5, 6, 8, 9, 10, 11] {
        assert!(instruction.accounts[index].is_optional);
        assert!(instruction.accounts[index].omitted);
        assert_eq!(
            instruction.accounts[index].address,
            JUPITER_LIQUIDITY_PROGRAM
        );
    }
    assert!(!instruction.accounts[4].omitted);
    assert_eq!(instruction.remaining_accounts, keys[13..]);
}

#[test]
fn retains_signed_zero_and_negative_amounts_and_all_transfer_variants() {
    let mut data = operate();
    data[8..24].copy_from_slice(&(-123i128).to_le_bytes());
    data[24..40].copy_from_slice(&0i128.to_le_bytes());
    for (variant, name) in [(0, "SKIP"), (1, "DIRECT"), (2, "CLAIM")] {
        data[104] = variant;
        let instruction = parse_jupiter_instruction(&data, &accounts(13))
            .unwrap()
            .unwrap();
        assert_eq!(instruction.arguments["supply_amount"], "-123");
        assert_eq!(instruction.arguments["borrow_amount"], "0");
        assert_eq!(instruction.arguments["transfer_type"], name);
    }
}

#[test]
fn rejects_known_layout_changes_and_unknown_transfer_variants() {
    let data = operate();
    assert!(parse_jupiter_instruction(&data[..104], &accounts(13)).is_err());
    let mut extended = data.clone();
    extended.push(0);
    assert!(parse_jupiter_instruction(&extended, &accounts(13)).is_err());
    let mut newer = data;
    newer[104] = 3;
    assert!(parse_jupiter_instruction(&newer, &accounts(13)).is_err());
}

#[test]
fn does_not_relabel_unsupported_version_or_event_as_instruction() {
    for data in [
        vec![],
        vec![217, 106],
        vec![0xff; 8],
        vec![180, 8, 81, 71, 19, 132, 173, 8],
    ] {
        assert!(parse_jupiter_instruction(&data, &[]).unwrap().is_none());
    }
}

#[test]
fn validates_known_account_count_and_all_account_keys() {
    assert!(parse_jupiter_instruction(&operate(), &accounts(12)).is_err());
    for index in [0, 5, 13] {
        let mut keys = accounts(14);
        keys[index] = "invalid-encoded-key".to_owned();
        assert!(parse_jupiter_instruction(&operate(), &keys).is_err());
    }
    let mut short = accounts(13);
    short[0] = bs58::encode([1; 31]).into_string();
    assert!(parse_jupiter_instruction(&operate(), &short).is_err());
}

#[test]
fn pre_operate_and_exchange_update_keep_mint_and_exact_roles() {
    for (discriminator, name, argument, count, reserve_index) in [
        (
            [129, 205, 158, 155, 198, 155, 72, 133],
            "pre_operate",
            "mint",
            7,
            5,
        ),
        (
            [239, 244, 10, 248, 116, 25, 53, 150],
            "update_exchange_price",
            "_mint",
            2,
            0,
        ),
    ] {
        let mut data = discriminator.to_vec();
        data.extend_from_slice(&[9; 32]);
        let instruction = parse_jupiter_instruction(&data, &accounts(count))
            .unwrap()
            .unwrap();
        assert_eq!(instruction.name, name);
        assert_eq!(
            instruction.arguments,
            json!({ argument: bs58::encode([9; 32]).into_string() })
        );
        assert_eq!(instruction.accounts[reserve_index].name, "token_reserve");
        assert!(instruction.requested_only);
        assert_eq!(instruction.arguments.as_object().unwrap().len(), 1);
        assert!(parse_jupiter_instruction(&data[..39], &accounts(count)).is_err());
    }
}

#[test]
fn rate_configuration_preserves_wide_arguments_and_official_role_names() {
    for (discriminator, name, fields) in [
        (
            [6, 20, 34, 122, 22, 150, 180, 22],
            "update_rate_data_v1",
            vec![
                "kink",
                "rate_at_utilization_zero",
                "rate_at_utilization_kink",
                "rate_at_utilization_max",
            ],
        ),
        (
            [116, 73, 53, 146, 216, 45, 228, 124],
            "update_rate_data_v2",
            vec![
                "kink1",
                "kink2",
                "rate_at_utilization_zero",
                "rate_at_utilization_kink1",
                "rate_at_utilization_kink2",
                "rate_at_utilization_max",
            ],
        ),
    ] {
        let mut data = discriminator.to_vec();
        for index in 0..fields.len() {
            data.extend_from_slice(&(u128::MAX - index as u128).to_le_bytes());
        }
        let instruction = parse_jupiter_instruction(&data, &accounts(5))
            .unwrap()
            .unwrap();
        assert_eq!(instruction.name, name);
        for (index, field) in fields.iter().enumerate() {
            assert_eq!(
                instruction.arguments["rate_data"][field],
                (u128::MAX - index as u128).to_string()
            );
        }
        assert_eq!(
            instruction
                .accounts
                .iter()
                .map(|account| account.name.as_str())
                .collect::<Vec<_>>(),
            [
                "authority",
                "auth_list",
                "rate_model",
                "mint",
                "token_reserve"
            ]
        );
        assert!(instruction.arguments.get("borrow_apr").is_none());
        assert!(parse_jupiter_instruction(&data[..data.len() - 1], &accounts(5)).is_err());
    }
}

#[test]
fn token_config_keeps_requested_fee_and_utilization_without_yield_claim() {
    let mut data = vec![231, 122, 181, 79, 255, 79, 144, 167];
    data.extend_from_slice(&[8; 32]);
    data.extend_from_slice(&123u128.to_le_bytes());
    data.extend_from_slice(&10_000u128.to_le_bytes());
    let instruction = parse_jupiter_instruction(&data, &accounts(5))
        .unwrap()
        .unwrap();
    assert_eq!(
        instruction.arguments,
        json!({ "token_config": {
        "token": bs58::encode([8; 32]).into_string(), "fee": "123", "max_utilization": "10000",
    } })
    );
    assert!(instruction.requested_only);
    assert!(instruction.arguments.get("apy").is_none());
}
