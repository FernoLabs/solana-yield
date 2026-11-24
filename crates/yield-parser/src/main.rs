use std::io::{self, Read, Write};
use std::process::ExitCode;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use yield_parser::{jupiter, kamino, ledger, Account, ShareValue};

mod live_batch;

const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum Input {
    Account(AccountInput),
    Ledger(LedgerInput),
    Live(live_batch::Input),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountInput {
    parser: Parser,
    account: EncodedAccount,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerInput {
    ledger_transaction: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Parser {
    KaminoReserve {
        lending_market: String,
        asset_mint: String,
        share_mint: String,
    },
    JupiterEarn {
        liquidity_reserve: String,
        asset_mint: String,
        share_mint: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedAccount {
    address: String,
    owner: String,
    executable: bool,
    lamports: u64,
    data_base64: String,
}

#[derive(Serialize)]
struct Output<'a> {
    schema_version: u8,
    method: &'static str,
    account: &'a str,
    asset_mint: &'a str,
    share_mint: &'a str,
    last_update_slot: Option<String>,
    last_update_unix_seconds: Option<String>,
    raw_assets_per_raw_share: Ratio,
}

#[derive(Serialize)]
struct Ratio {
    numerator: String,
    denominator: String,
}

impl From<ShareValue> for Ratio {
    fn from(value: ShareValue) -> Self {
        Self {
            numerator: value.numerator().to_string(),
            denominator: value.denominator().to_string(),
        }
    }
}

fn run(mut reader: impl Read, mut writer: impl Write) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("input exceeds 2 MiB".into());
    }

    match serde_json::from_slice(&bytes)? {
        Input::Account(input) => serde_json::to_writer(&mut writer, &share_value(&input)?)?,
        Input::Ledger(input) => serde_json::to_writer(
            &mut writer,
            &ledger::parse_transaction(&input.ledger_transaction)?,
        )?,
        Input::Live(input) => serde_json::to_writer(&mut writer, &live_batch::parse(&input)?)?,
    }
    writer.write_all(b"\n")?;
    Ok(())
}

fn share_value(input: &AccountInput) -> Result<Output<'_>, Box<dyn std::error::Error>> {
    let data = STANDARD.decode(&input.account.data_base64)?;
    let account = Account {
        address: &input.account.address,
        owner: &input.account.owner,
        executable: input.account.executable,
        lamports: input.account.lamports,
        data: &data,
    };
    let (method, asset_mint, share_mint, value) = match &input.parser {
        Parser::KaminoReserve {
            lending_market,
            asset_mint,
            share_mint,
        } => (
            "kamino_lend_reserve_v1_stored_share_value",
            asset_mint,
            share_mint,
            kamino::share_value(&account, lending_market, asset_mint, share_mint)?,
        ),
        Parser::JupiterEarn {
            liquidity_reserve,
            asset_mint,
            share_mint,
        } => (
            "jupiter_earn_stored_share_value",
            asset_mint,
            share_mint,
            jupiter::share_value(&account, liquidity_reserve, asset_mint, share_mint)?,
        ),
    };
    Ok(Output {
        schema_version: 1,
        method,
        account: &input.account.address,
        asset_mint,
        share_mint,
        last_update_slot: value.last_update_slot().map(|slot| slot.to_string()),
        last_update_unix_seconds: value
            .last_update_unix_seconds()
            .map(|time| time.to_string()),
        raw_assets_per_raw_share: value.into(),
    })
}

fn main() -> ExitCode {
    match run(io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yield-parser: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KAMINO_FIXTURE: &[u8] = include_bytes!("../fixtures/kamino-reserve-usdg.json");

    #[test]
    fn cli_input_from_a_public_reserve_preserves_the_exact_ratio() {
        let mut output = Vec::new();
        run(KAMINO_FIXTURE, &mut output).unwrap();
        let answer: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(answer["schema_version"], 1);
        assert_eq!(
            answer["method"],
            "kamino_lend_reserve_v1_stored_share_value"
        );
        assert_eq!(answer["last_update_slot"], "452570160");
        assert_eq!(answer["last_update_unix_seconds"], "1790933433");
        assert_eq!(
            answer["raw_assets_per_raw_share"],
            serde_json::json!({
                "numerator": "50979573413033157950669539918836",
                "denominator": "46806536763770400250274189934592"
            })
        );
    }

    #[test]
    fn refuses_invalid_account_data_without_emitting_a_value() {
        let mut input: serde_json::Value = serde_json::from_slice(KAMINO_FIXTURE).unwrap();
        input["account"]["data_base64"] = serde_json::json!("@");
        let bytes = serde_json::to_vec(&input).unwrap();
        let mut output = Vec::new();
        assert!(run(bytes.as_slice(), &mut output).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn refuses_unbounded_input() {
        let bytes = vec![b' '; MAX_INPUT_BYTES + 1];
        let mut output = Vec::new();
        let error = run(bytes.as_slice(), &mut output).unwrap_err();
        assert_eq!(error.to_string(), "input exceeds 2 MiB");
        assert!(output.is_empty());
    }
}
