use std::io::{self, Read, Write};
use std::process::ExitCode;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use yield_parser::{jupiter, kamino, ledger, Account, ShareValue};

mod live_batch;
#[cfg(test)]
mod watch_tests;

const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum Input {
    Account(AccountInput),
    Ledger(LedgerInput),
    Live(live_batch::Input),
    Watch(live_batch::WatchInput),
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
        Input::Watch(input) => serde_json::to_writer(&mut writer, &live_batch::describe(&input)?)?,
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

    fn live_bank(slot: u64, seconds: i64) -> serde_json::Value {
        let mut clock = [0_u8; 40];
        clock[..8].copy_from_slice(&slot.to_le_bytes());
        clock[32..].copy_from_slice(&seconds.to_le_bytes());
        serde_json::json!({"live_bank": {
            "slot": slot,
            "entries": {yield_parser::live::CLOCK: {
                "owner": "Sysvar1111111111111111111111111111111111111",
                "executable": false, "lamports": 1, "rent_epoch": 0,
                "data_base64": STANDARD.encode(clock)
            }},
            "markets": []
        }})
    }

    #[test]
    fn batch_uses_the_shared_adapter_and_keeps_unavailable_rows() {
        let source: serde_json::Value = serde_json::from_slice(KAMINO_FIXTURE).unwrap();
        let mut input = live_bank(452_570_160, 1_790_933_433);
        let address = source["account"]["address"].as_str().unwrap();
        let mut record = source["account"].clone();
        record.as_object_mut().unwrap().remove("address");
        record["rent_epoch"] = serde_json::json!(0);
        input["live_bank"]["entries"][address] = record;
        input["live_bank"]["markets"] = serde_json::json!([
            {"id":"kamino-usdg", "kind":"kamino_reserve", "accounts": {
                "reserve": address,
                "lending_market": source["parser"]["lending_market"],
                "asset_mint": source["parser"]["asset_mint"],
                "share_mint": source["parser"]["share_mint"]
            }},
            {"id":"missing-save", "kind":"save", "accounts": {
                "reserve":"BJfY2E6TVEQe896kfR1AhhYCECVKfbceGg2ASU2LXdiA",
                "lending_market":"7XttJ7hp83u5euzT7ybC5zsjdgKA4WPbQHVS27CATAJH",
                "asset_mint":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
            }}
        ]);
        let bytes = serde_json::to_vec(&input).unwrap();
        let mut output = Vec::new();
        run(bytes.as_slice(), &mut output).unwrap();
        let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(output["slot"], 452_570_160_u64);
        assert_eq!(output["rows"][0]["id"], "kamino-usdg");
        assert_eq!(output["rows"][0]["venue"], "kamino");
        assert_eq!(output["rows"][0]["unavailable"], serde_json::Value::Null);
        assert_eq!(
            output["rows"][0]["values"][0],
            serde_json::json!({
                "kind":"stored_assets_per_share",
                "numerator":"50979573413033157950669539918836",
                "denominator":"46806536763770400250274189934592",
                "updated_slot":452_570_160_u64,
                "updated_unix_seconds":1_790_933_433_u64
            })
        );
        assert_eq!(output["rows"][1]["id"], "missing-save");
        assert_eq!(output["rows"][1]["venue"], "save");
        assert_eq!(output["rows"][1]["values"], serde_json::json!([]));
        assert!(output["rows"][1]["unavailable"].is_string());

        input["live_bank"]["slot"] = serde_json::json!(452_570_161_u64);
        let bytes = serde_json::to_vec(&input).unwrap();
        let mut output = Vec::new();
        assert!(run(bytes.as_slice(), &mut output).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn jupiter_batch_preserves_its_stored_share_value() {
        const ADDRESS: &str = "2vVYHYM8VYnvZqQWpTJSj8o8DBf1wM8pVs3bsTgYZiqJ";
        let mut input = live_bank(463_283_500, 1_793_362_942);
        input["live_bank"]["entries"][ADDRESS] = serde_json::json!({
            "owner":"jup3YeL8QhtSx1e253b2FDvsMNC87fDrgQZivbrndc9",
            "executable":false,"lamports":1,"rent_epoch":0,
            "data_base64":STANDARD.encode(include_bytes!("../fixtures/jupiter-earn-usdc-lending.bin"))
        });
        input["live_bank"]["markets"] = serde_json::json!([
            {"id":"jupiter-usdc","kind":"jupiter_earn","accounts":{
                "lending":ADDRESS,
                "liquidity_reserve":"94vK29npVbyRHXH63rRcTiSr26SFhrQTzbpNJuhQEDu",
                "asset_mint":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "share_mint":"9BEcn9aPEmhSPbPQeFGjidRiEKki46fVQDyPpSQXPA2D"
            }}
        ]);
        let bytes = serde_json::to_vec(&input).unwrap();
        let mut output = Vec::new();
        run(bytes.as_slice(), &mut output).unwrap();
        let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(output["rows"][0]["venue"], "jupiter");
        assert_eq!(output["rows"][0]["unavailable"], serde_json::Value::Null);
        assert_eq!(output["rows"][0]["values"][0]["numerator"], "1067222452096");
        assert_eq!(
            output["rows"][0]["values"][0]["denominator"],
            "1000000000000"
        );
        assert_eq!(
            output["rows"][0]["values"][0]["updated_slot"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn existing_elemental_batch_keeps_its_posted_zero_rate_and_output_shape() {
        const ADDRESS: &str = "5XCP3oD3JAuQyDpfBFFVUxsBxNjPQojpKuL4aVhHsDok";
        const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let mut data = [0_u8; 254];
        data[..8].copy_from_slice(&[241, 154, 109, 4, 17, 177, 109, 188]);
        data[8..40].copy_from_slice(&bs58::decode(MINT).into_vec().unwrap());
        data[72..80].copy_from_slice(&1_000_000_u64.to_le_bytes());
        data[96..104].copy_from_slice(&100_u64.to_le_bytes());
        let mut input = live_bank(100, 1_000);
        input["live_bank"]["entries"][ADDRESS] = serde_json::json!({
            "owner":"ELE5vYY81W7UCpTPs7SyD6Bwm5FwZBntTW8PiGqM5d4A",
            "executable":false,"lamports":1,"rent_epoch":0,"data_base64":STANDARD.encode(data)
        });
        input["live_bank"]["markets"] = serde_json::json!([
            {"id":"elemental-usdc","kind":"elemental","accounts":{"pool":ADDRESS,"asset_mint":MINT}}
        ]);
        let bytes = serde_json::to_vec(&input).unwrap();
        let mut output = Vec::new();
        run(bytes.as_slice(), &mut output).unwrap();
        let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            output["rows"],
            serde_json::json!([
                {"id":"elemental-usdc","venue":"elemental","values":[{
                    "kind":"posted_apr","numerator":"0","denominator":"1000000000",
                    "updated_slot":null,"updated_unix_seconds":null
                }],"unavailable":null}
            ])
        );
    }
}
