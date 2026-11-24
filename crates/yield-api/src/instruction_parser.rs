//! Jupiter Lend liquidity instruction bytes, independent of execution logs.
//!
//! Layouts and account order are pinned to the official Jupiter IDL:
//! <https://github.com/jup-ag/jupiter-lend/blob/33a22cf7a5bfdd32ab1712dda4adfbeb9b348ad9/target/idl/liquidity.json>.
//! Optional account placeholders follow Anchor's program-ID sentinel rule:
//! <https://github.com/coral-xyz/anchor/blob/v0.31.1/lang/src/accounts/option.rs>.
//!
//! This module decodes requested arguments. It does not establish execution,
//! transferred amounts, resulting account state, an exchange price, or a yield.
//! The caller must establish the exact program ID, preserve transaction and
//! instruction positions, and separately record success and bank finality.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::stream_parser::JUPITER_LIQUIDITY_PROGRAM;

const OPERATE: [u8; 8] = [217, 106, 208, 99, 116, 151, 42, 135];
const PRE_OPERATE: [u8; 8] = [129, 205, 158, 155, 198, 155, 72, 133];
const UPDATE_EXCHANGE_PRICE: [u8; 8] = [239, 244, 10, 248, 116, 25, 53, 150];
const UPDATE_RATE_DATA_V1: [u8; 8] = [6, 20, 34, 122, 22, 150, 180, 22];
const UPDATE_RATE_DATA_V2: [u8; 8] = [116, 73, 53, 146, 216, 45, 228, 124];
const UPDATE_TOKEN_CONFIG: [u8; 8] = [231, 122, 181, 79, 255, 79, 144, 167];

const OPERATE_ACCOUNTS: &[(&str, bool)] = &[
    ("protocol", false),
    ("liquidity", false),
    ("token_reserve", false),
    ("mint", false),
    ("vault", false),
    ("user_supply_position", true),
    ("user_borrow_position", true),
    ("rate_model", false),
    ("withdraw_to_account", true),
    ("borrow_to_account", true),
    ("borrow_claim_account", true),
    ("withdraw_claim_account", true),
    ("token_program", false),
];
const PRE_OPERATE_ACCOUNTS: &[(&str, bool)] = &[
    ("protocol", false),
    ("liquidity", false),
    ("user_supply_position", true),
    ("user_borrow_position", true),
    ("vault", false),
    ("token_reserve", false),
    ("token_program", false),
];
const UPDATE_EXCHANGE_PRICE_ACCOUNTS: &[(&str, bool)] =
    &[("token_reserve", false), ("rate_model", false)];
const CONFIG_ACCOUNTS: &[(&str, bool)] = &[
    ("authority", false),
    ("auth_list", false),
    ("rate_model", false),
    ("mint", false),
    ("token_reserve", false),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JupiterInstructionAccount {
    /// Exact official IDL account name, in instruction order.
    pub name: String,
    /// Exact resolved key, including the program-ID placeholder when omitted.
    pub address: String,
    pub is_optional: bool,
    /// True only for an optional account set to the liquidity program ID.
    pub omitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JupiterInstruction {
    pub name: String,
    /// Official argument names. All i128 and u128 values are decimal strings.
    /// An amount sentinel remains the original requested integer; it must not
    /// be reported as a transferred amount without separate execution evidence.
    pub arguments: Value,
    pub accounts: Vec<JupiterInstructionAccount>,
    /// Keys after the known IDL accounts, with no invented role labels.
    pub remaining_accounts: Vec<String>,
    /// The decoder alone does not prove execution or a resulting state change.
    pub requested_only: bool,
}

/// Decode one instruction of the exact Jupiter liquidity program.
///
/// `accounts` contains resolved addresses in the instruction's account-index
/// order, including address-table keys and optional program-ID placeholders.
/// Unknown discriminators, including short data, return `Ok(None)`. A known
/// instruction with an incompatible layout returns an error. In both cases the
/// caller must retain the raw instruction and must not drop the transaction.
/// No runtime log is needed or consulted.
pub fn parse_jupiter_instruction(
    data: &[u8],
    accounts: &[String],
) -> Result<Option<JupiterInstruction>> {
    let Some(discriminator) = data.get(..8) else {
        return Ok(None);
    };
    let (name, length, layout): (&str, usize, &[(&str, bool)]) = match discriminator {
        value if value == OPERATE => ("operate", 105, OPERATE_ACCOUNTS),
        value if value == PRE_OPERATE => ("pre_operate", 40, PRE_OPERATE_ACCOUNTS),
        value if value == UPDATE_EXCHANGE_PRICE => {
            ("update_exchange_price", 40, UPDATE_EXCHANGE_PRICE_ACCOUNTS)
        }
        value if value == UPDATE_RATE_DATA_V1 => ("update_rate_data_v1", 72, CONFIG_ACCOUNTS),
        value if value == UPDATE_RATE_DATA_V2 => ("update_rate_data_v2", 104, CONFIG_ACCOUNTS),
        value if value == UPDATE_TOKEN_CONFIG => ("update_token_config", 72, CONFIG_ACCOUNTS),
        _ => return Ok(None),
    };
    ensure!(
        data.len() == length,
        "invalid Jupiter {name} instruction length"
    );
    ensure!(
        accounts.len() >= layout.len(),
        "insufficient Jupiter {name} instruction accounts"
    );
    for account in accounts {
        let mut decoded = [0u8; 32];
        ensure!(
            bs58::decode(account)
                .onto(&mut decoded)
                .context("invalid Jupiter instruction account address")?
                == 32,
            "invalid Jupiter instruction account address length"
        );
    }

    // Exact length checks above establish all byte ranges below.
    let key = |at| bs58::encode(&data[at..at + 32]).into_string();
    let signed = |at| i128::from_le_bytes(data[at..at + 16].try_into().unwrap()).to_string();
    let unsigned = |at| u128::from_le_bytes(data[at..at + 16].try_into().unwrap()).to_string();
    let fields = |names: &[&str]| -> Value {
        Value::Object(
            names
                .iter()
                .enumerate()
                .map(|(index, name)| ((*name).to_owned(), Value::String(unsigned(8 + 16 * index))))
                .collect::<Map<_, _>>(),
        )
    };
    let arguments = match name {
        "operate" => {
            let transfer_type = match data[104] {
                0 => "SKIP",
                1 => "DIRECT",
                2 => "CLAIM",
                _ => anyhow::bail!("unsupported Jupiter operate transfer_type variant"),
            };
            json!({
                "supply_amount": signed(8),
                "borrow_amount": signed(24),
                "withdraw_to": key(40),
                "borrow_to": key(72),
                "transfer_type": transfer_type,
            })
        }
        "pre_operate" => json!({ "mint": key(8) }),
        "update_exchange_price" => json!({ "_mint": key(8) }),
        "update_rate_data_v1" => json!({ "rate_data": fields(&[
            "kink", "rate_at_utilization_zero", "rate_at_utilization_kink", "rate_at_utilization_max",
        ]) }),
        "update_rate_data_v2" => json!({ "rate_data": fields(&[
            "kink1", "kink2", "rate_at_utilization_zero", "rate_at_utilization_kink1",
            "rate_at_utilization_kink2", "rate_at_utilization_max",
        ]) }),
        "update_token_config" => json!({ "token_config": {
            "token": key(8), "fee": unsigned(40), "max_utilization": unsigned(56),
        } }),
        _ => unreachable!("all recognized Jupiter methods have an argument decoder"),
    };
    let labeled = layout
        .iter()
        .zip(accounts)
        .map(|((name, is_optional), address)| JupiterInstructionAccount {
            name: (*name).to_owned(),
            address: address.clone(),
            is_optional: *is_optional,
            omitted: *is_optional && address == JUPITER_LIQUIDITY_PROGRAM,
        })
        .collect();
    Ok(Some(JupiterInstruction {
        name: name.to_owned(),
        arguments,
        accounts: labeled,
        remaining_accounts: accounts[layout.len()..].to_vec(),
        requested_only: true,
    }))
}
