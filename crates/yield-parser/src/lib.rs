//! Parse stored Solana account values. The caller supplies the account bytes
//! and verifies the chain, bank, and time from which they came.

pub mod adapter;
pub mod protocols;

// Keep the original public imports valid for existing collectors.
pub use protocols::jupiter::ledger as jupiter_ledger;
pub use protocols::kamino::ledger as kamino_ledger;
pub use protocols::{jupiter, kamino};
#[cfg(feature = "ledger")]
pub mod ledger;
pub mod live;
pub use protocols::project0 as live_project0;
pub use protocols::save as live_save;
pub mod live_vaults;

use core::fmt;

/// Values explicitly emitted by a supported program. These are observations
/// at the instruction position, with no assumption about intervening updates.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ledger", derive(serde::Serialize))]
#[cfg_attr(feature = "ledger", serde(tag = "kind", rename_all = "snake_case"))]
pub enum LedgerMeasurement {
    JupiterEarnExchangePrices {
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u64"))]
        token_exchange_price: u64,
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u64"))]
        liquidity_exchange_price: u64,
    },
    JupiterLiquidityExchangePrices {
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_key"))]
        token: [u8; 32],
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u128"))]
        supply_exchange_price: u128,
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u128"))]
        borrow_exchange_price: u128,
        borrow_rate_bps: u16,
        utilization_bps: u16,
    },
    JupiterLiquidityOperationPrices {
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_key"))]
        token: [u8; 32],
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u64"))]
        supply_exchange_price: u64,
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u64"))]
        borrow_exchange_price: u64,
    },
    KaminoReserveExchangePrice {
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_key"))]
        reserve: [u8; 32],
        #[cfg_attr(feature = "ledger", serde(serialize_with = "ledger::serialize_u128"))]
        exchange_rate_sf: u128,
        liquidity_mint_decimals: u8,
    },
}

/// One account from a caller-verified snapshot.
#[derive(Clone, Copy, Debug)]
pub struct Account<'a> {
    pub address: &'a str,
    pub owner: &'a str,
    pub executable: bool,
    pub lamports: u64,
    pub data: &'a [u8],
}

/// Exact ratio in raw asset units per raw share unit, with the program's
/// stored update fields when available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShareValue {
    numerator: u128,
    denominator: u128,
    last_update_slot: Option<u64>,
    last_update_unix_seconds: Option<u64>,
}

impl ShareValue {
    pub(crate) fn new(
        numerator: u128,
        denominator: u128,
        last_update_slot: Option<u64>,
        last_update_unix_seconds: Option<u64>,
    ) -> Result<Self, ParseError> {
        if numerator == 0 || denominator == 0 {
            return Err(ParseError::InvalidValue);
        }
        Ok(Self {
            numerator,
            denominator,
            last_update_slot,
            last_update_unix_seconds,
        })
    }

    pub const fn numerator(self) -> u128 {
        self.numerator
    }

    pub const fn denominator(self) -> u128 {
        self.denominator
    }

    /// The program's stored update slot, when the layout records it.
    pub const fn last_update_slot(self) -> Option<u64> {
        self.last_update_slot
    }

    /// The program's stored update time in Unix seconds.
    pub const fn last_update_unix_seconds(self) -> Option<u64> {
        self.last_update_unix_seconds
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    InvalidAccount,
    UnsupportedLayout,
    AccountMismatch,
    InvalidValue,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidAccount => "invalid account identity, owner, or state",
            Self::UnsupportedLayout => "unsupported account layout",
            Self::AccountMismatch => "account does not match the expected market or mints",
            Self::InvalidValue => "invalid value in account data",
        })
    }
}

impl std::error::Error for ParseError {}

pub(crate) fn key(value: &str) -> Result<[u8; 32], ParseError> {
    if !(32..=44).contains(&value.len()) {
        return Err(ParseError::InvalidAccount);
    }
    let mut bytes = [0_u8; 32];
    if bs58::decode(value).onto(&mut bytes) != Ok(bytes.len()) {
        return Err(ParseError::InvalidAccount);
    }
    Ok(bytes)
}

pub(crate) fn body<'a>(
    account: &Account<'a>,
    owner: &str,
    size: usize,
    discriminator: [u8; 8],
) -> Result<&'a [u8], ParseError> {
    key(account.address)?;
    if account.owner != owner || account.executable || account.lamports == 0 {
        return Err(ParseError::InvalidAccount);
    }
    if account.data.len() != size || !account.data.starts_with(&discriminator) {
        return Err(ParseError::UnsupportedLayout);
    }
    Ok(account.data)
}
