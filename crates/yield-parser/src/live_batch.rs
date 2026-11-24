use std::collections::{BTreeMap, BTreeSet};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use yield_parser::{live, live_project0, live_save, live_vaults, Account, ParseError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    live_bank: Bank,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bank {
    slot: u64,
    entries: BTreeMap<String, Option<Record>>,
    markets: Vec<Market>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    owner: String,
    executable: bool,
    lamports: u64,
    rent_epoch: u64,
    data_base64: String,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "accounts",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Venue {
    Save {
        reserve: String,
        lending_market: String,
        asset_mint: String,
    },
    Project0 {
        bank: String,
        group: String,
        asset_mint: String,
    },
    Elemental {
        pool: String,
        asset_mint: String,
    },
    Hylo {
        config: String,
        pool: String,
        share_mint: String,
    },
    Meteora {
        vault: String,
        share_mint: String,
        asset_mint: String,
    },
}

#[derive(Deserialize)]
struct Market {
    id: String,
    #[serde(flatten)]
    venue: Venue,
}

#[derive(Serialize)]
pub(crate) struct Batch<'a> {
    schema_version: u8,
    slot: u64,
    unix_seconds: u64,
    rows: Vec<Row<'a>>,
}

#[derive(Serialize)]
struct Row<'a> {
    id: &'a str,
    venue: &'static str,
    values: Vec<Value>,
    unavailable: Option<String>,
}

#[derive(Serialize)]
struct Value {
    kind: &'static str,
    numerator: String,
    denominator: String,
    updated_slot: Option<u64>,
    updated_unix_seconds: Option<u64>,
}

impl From<live::Measure> for Value {
    fn from(value: live::Measure) -> Self {
        Self {
            kind: value.kind,
            numerator: value.numerator.to_string(),
            denominator: value.denominator.to_string(),
            updated_slot: value.updated_slot,
            updated_unix_seconds: value.updated_unix_seconds,
        }
    }
}

struct Decoded<'a> {
    record: &'a Record,
    data: Vec<u8>,
}

pub(crate) fn parse(input: &Input) -> Result<Batch<'_>, Box<dyn std::error::Error>> {
    let bank = &input.live_bank;
    if bank.slot == 0
        || bank.markets.is_empty()
        || bank.markets.len() > 100
        || bank.entries.len() > 100
    {
        return Err("invalid live Bank batch size or slot".into());
    }
    let mut records = BTreeMap::new();
    for (key, record) in &bank.entries {
        if let Some(record) = record {
            // The frame verifier includes rent_epoch in its digest. Yield methods
            // use the authenticated owner, balance and bytes.
            let _ = record.rent_epoch;
            let data = STANDARD.decode(&record.data_base64)?;
            records.insert(key.as_str(), Decoded { record, data });
        }
    }
    let get = |key: &str| -> Result<Account<'_>, ParseError> {
        let (key, value) = records
            .get_key_value(key)
            .ok_or(ParseError::InvalidAccount)?;
        Ok(Account {
            address: key,
            owner: &value.record.owner,
            executable: value.record.executable,
            lamports: value.record.lamports,
            data: &value.data,
        })
    };
    let time = live::clock_unix_seconds(&get(live::CLOCK)?, bank.slot)?;
    let mut ids = BTreeSet::new();
    let mut rows = Vec::new();
    for market in &bank.markets {
        if market.id.is_empty() || market.id.len() > 160 || !ids.insert(&market.id) {
            return Err("invalid or duplicate market ID".into());
        }
        let (venue, result) = market.venue.observe(&get, time);
        let result = result.and_then(|values| {
            if values.is_empty()
                || values.iter().any(|value| {
                    value.denominator == 0
                        || value.updated_slot.is_some_and(|slot| slot > bank.slot)
                        || value
                            .updated_unix_seconds
                            .is_some_and(|updated| updated > time)
                })
            {
                return Err(ParseError::InvalidValue);
            }
            Ok(values)
        });
        let (values, unavailable) = match result {
            Ok(values) => (values.into_iter().map(Value::from).collect(), None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        rows.push(Row {
            id: &market.id,
            venue,
            values,
            unavailable,
        });
    }
    Ok(Batch {
        schema_version: 1,
        slot: bank.slot,
        unix_seconds: time,
        rows,
    })
}

impl Venue {
    fn observe<'a>(
        &self,
        get: &impl Fn(&str) -> Result<Account<'a>, ParseError>,
        time: u64,
    ) -> (&'static str, Result<Vec<live::Measure>, ParseError>) {
        match self {
            Self::Save {
                reserve,
                lending_market,
                asset_mint,
            } => (
                "save",
                get(reserve)
                    .and_then(|account| live_save::observe(&account, lending_market, asset_mint)),
            ),
            Self::Project0 {
                bank,
                group,
                asset_mint,
            } => (
                "project0",
                get(bank).and_then(|account| live_project0::observe(&account, group, asset_mint)),
            ),
            Self::Elemental { pool, asset_mint } => (
                "elemental",
                get(pool).and_then(|account| live_vaults::observe_elemental(&account, asset_mint)),
            ),
            Self::Hylo {
                config,
                pool,
                share_mint,
            } => (
                "hylo",
                (|| live_vaults::observe_hylo(&get(config)?, &get(pool)?, &get(share_mint)?))(),
            ),
            Self::Meteora {
                vault,
                share_mint,
                asset_mint,
            } => (
                "meteora",
                (|| {
                    live_vaults::observe_meteora(&get(vault)?, &get(share_mint)?, asset_mint, time)
                })(),
            ),
        }
    }
}
