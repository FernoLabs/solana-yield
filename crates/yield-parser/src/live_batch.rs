use std::collections::{BTreeMap, BTreeSet};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use yield_parser::{
    adapter::{self, AccountProvider, ObservationContext, ProtocolAdapter},
    protocols::{elemental, hylo, jupiter, kamino, meteora, project0, save},
    Account, ParseError,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    live_bank: Bank,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WatchInput {
    watch_list: WatchList,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchList {
    markets: Vec<Market>,
}

#[derive(Serialize)]
pub(crate) struct WatchDescription {
    schema_version: u8,
    keys: Vec<String>,
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
    #[serde(alias = "kamino")]
    KaminoReserve {
        reserve: String,
        lending_market: String,
        asset_mint: String,
        share_mint: String,
    },
    #[serde(alias = "jupiter")]
    JupiterEarn {
        lending: String,
        liquidity_reserve: String,
        asset_mint: String,
        share_mint: String,
    },
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

fn validate_markets(markets: &[Market]) -> Result<(), &'static str> {
    if markets.is_empty() || markets.len() > 100 {
        return Err("market list must contain 1 to 100 entries");
    }
    let mut ids = BTreeSet::new();
    for market in markets {
        if market.id.trim().is_empty()
            || market.id.len() > 160
            || market.id.chars().any(char::is_control)
            || !ids.insert(&market.id)
        {
            return Err("invalid or duplicate market ID");
        }
    }
    Ok(())
}

/// Describe the accounts needed by the same adapters used for Bank parsing.
/// This command reads no account bytes and performs no network request.
pub(crate) fn describe(input: &WatchInput) -> Result<WatchDescription, Box<dyn std::error::Error>> {
    validate_markets(&input.watch_list.markets)?;
    let mut keys = BTreeSet::from([yield_parser::live::CLOCK.to_owned()]);
    for market in &input.watch_list.markets {
        let protocol = market.venue.adapter();
        for account in protocol.required_accounts() {
            let mut decoded = [0_u8; 32];
            if !(32..=44).contains(&account.address.len())
                || bs58::decode(account.address).onto(&mut decoded) != Ok(decoded.len())
            {
                return Err("adapter returned an invalid account public key".into());
            }
            keys.insert(account.address.to_owned());
        }
    }
    if keys.len() > adapter::MAX_ACCOUNTS {
        return Err("watch list exceeds the account limit".into());
    }
    Ok(WatchDescription {
        schema_version: 1,
        keys: keys.into_iter().collect(),
    })
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

impl From<yield_parser::live::Measure> for Value {
    fn from(value: yield_parser::live::Measure) -> Self {
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

struct BankAccounts<'a> {
    records: BTreeMap<&'a str, Decoded<'a>>,
}

impl AccountProvider for BankAccounts<'_> {
    fn account(&self, key: &str) -> Result<Account<'_>, ParseError> {
        let (key, value) = self
            .records
            .get_key_value(key)
            .ok_or(ParseError::InvalidAccount)?;
        Ok(Account {
            address: key,
            owner: &value.record.owner,
            executable: value.record.executable,
            lamports: value.record.lamports,
            data: &value.data,
        })
    }
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
    let accounts = BankAccounts { records };
    let context = ObservationContext::new(bank.slot, &accounts)?;
    let mut ids = BTreeSet::new();
    let mut rows = Vec::new();
    for market in &bank.markets {
        if market.id.is_empty() || market.id.len() > 160 || !ids.insert(&market.id) {
            return Err("invalid or duplicate market ID".into());
        }
        let protocol = market.venue.adapter();
        let result = adapter::observe(protocol.as_ref(), &context);
        let (values, unavailable) = match result {
            Ok(values) => (values.into_iter().map(Value::from).collect(), None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        rows.push(Row {
            id: &market.id,
            venue: protocol.protocol(),
            values,
            unavailable,
        });
    }
    Ok(Batch {
        schema_version: 1,
        slot: bank.slot,
        unix_seconds: context.unix_seconds(),
        rows,
    })
}

impl Venue {
    // This is only JSON CLI registration. Library users can supply any adapter.
    fn adapter(&self) -> Box<dyn ProtocolAdapter + '_> {
        match self {
            Self::KaminoReserve {
                reserve,
                lending_market,
                asset_mint,
                share_mint,
            } => Box::new(kamino::Adapter {
                reserve,
                lending_market,
                asset_mint,
                share_mint,
            }),
            Self::JupiterEarn {
                lending,
                liquidity_reserve,
                asset_mint,
                share_mint,
            } => Box::new(jupiter::Adapter {
                lending,
                liquidity_reserve,
                asset_mint,
                share_mint,
            }),
            Self::Save {
                reserve,
                lending_market,
                asset_mint,
            } => Box::new(save::Adapter {
                reserve,
                lending_market,
                asset_mint,
            }),
            Self::Project0 {
                bank,
                group,
                asset_mint,
            } => Box::new(project0::Adapter {
                bank,
                group,
                asset_mint,
            }),
            Self::Elemental { pool, asset_mint } => {
                Box::new(elemental::Adapter { pool, asset_mint })
            }
            Self::Hylo {
                config,
                pool,
                share_mint,
            } => Box::new(hylo::Adapter {
                config,
                pool,
                share_mint,
            }),
            Self::Meteora {
                vault,
                share_mint,
                asset_mint,
            } => Box::new(meteora::Adapter {
                vault,
                share_mint,
                asset_mint,
            }),
        }
    }
}
