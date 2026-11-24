//! Common account-parser contract.
//!
//! The caller supplies an immutable account set from one externally verified
//! Bank. Matching Clock data checks the slot; it does not authenticate the
//! source or prove that the other accounts came from that Bank.

use std::collections::BTreeSet;

use crate::{key, live, Account, ParseError};

/// Maximum number of accounts in one parser input, including Clock.
pub const MAX_ACCOUNTS: usize = 100;
/// Maximum number of distinct measurements returned by one adapter.
pub const MAX_MEASUREMENTS: usize = 32;

/// An immutable view of accounts from one externally verified Bank.
///
/// Implementations must return the same bytes throughout an observation. The
/// caller verifies source identity, Bank provenance, integrity, and freshness
/// before constructing this view. This trait performs no network reads.
pub trait AccountProvider {
    fn account(&self, address: &str) -> Result<Account<'_>, ParseError>;
}

/// A bounded account provider for callers that already hold account records.
pub struct AccountSet<'a> {
    accounts: &'a [Account<'a>],
}

impl<'a> AccountSet<'a> {
    pub fn new(accounts: &'a [Account<'a>]) -> Result<Self, ParseError> {
        if accounts.is_empty() || accounts.len() > MAX_ACCOUNTS {
            return Err(ParseError::InvalidAccount);
        }
        let mut keys = BTreeSet::new();
        for account in accounts {
            key(account.address)?;
            if !keys.insert(account.address) {
                return Err(ParseError::AccountMismatch);
            }
        }
        Ok(Self { accounts })
    }
}

impl AccountProvider for AccountSet<'_> {
    fn account(&self, address: &str) -> Result<Account<'_>, ParseError> {
        self.accounts
            .iter()
            .find(|account| account.address == address)
            .copied()
            .ok_or(ParseError::InvalidAccount)
    }
}

/// One account that an adapter reads. Configuration-only keys such as an
/// expected mint are validated by the protocol reader and need not be fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountRequirement<'a> {
    pub role: &'static str,
    pub address: &'a str,
}

/// Shared slot and Clock checks for one immutable Bank input.
pub struct ObservationContext<'a> {
    slot: u64,
    unix_seconds: u64,
    provider: &'a dyn AccountProvider,
    allowed_accounts: Option<BTreeSet<String>>,
}

impl<'a> ObservationContext<'a> {
    pub fn new(slot: u64, provider: &'a dyn AccountProvider) -> Result<Self, ParseError> {
        if slot == 0 {
            return Err(ParseError::InvalidValue);
        }
        let clock = provider.account(live::CLOCK)?;
        let unix_seconds = live::clock_unix_seconds(&clock, slot)?;
        Ok(Self {
            slot,
            unix_seconds,
            provider,
            allowed_accounts: None,
        })
    }

    pub const fn slot(&self) -> u64 {
        self.slot
    }

    pub const fn unix_seconds(&self) -> u64 {
        self.unix_seconds
    }

    /// Check the requested identity and initialized account envelope. The
    /// protocol reader still checks its exact owner, layout, and key bindings.
    pub fn account(&self, address: &str) -> Result<Account<'_>, ParseError> {
        key(address)?;
        if address != live::CLOCK
            && self
                .allowed_accounts
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(address))
        {
            return Err(ParseError::AccountMismatch);
        }
        let account = self.provider.account(address)?;
        if account.address != address {
            return Err(ParseError::AccountMismatch);
        }
        key(account.owner)?;
        if account.executable || account.lamports == 0 {
            return Err(ParseError::InvalidAccount);
        }
        Ok(account)
    }
}

/// Extend the parser without editing a central protocol enum.
///
/// Implement the protocol's account requirements and pure calculation, then
/// call [`observe`] to apply the common input and output checks. A CLI can
/// register a JSON configuration separately from this library contract.
pub trait ProtocolAdapter {
    fn protocol(&self) -> &'static str;
    fn required_accounts(&self) -> Vec<AccountRequirement<'_>>;
    fn read(&self, context: &ObservationContext<'_>) -> Result<Vec<live::Measure>, ParseError>;
}

/// Run an adapter under the common contract. Valid zero values and absent
/// protocol update times are preserved. No APR, APY, or freshness is inferred.
pub fn observe(
    adapter: &dyn ProtocolAdapter,
    context: &ObservationContext<'_>,
) -> Result<Vec<live::Measure>, ParseError> {
    if !identifier(adapter.protocol()) {
        return Err(ParseError::InvalidValue);
    }
    let required = adapter.required_accounts();
    // Clock is mandatory in addition to the adapter's declared accounts.
    if required.is_empty() || required.len() >= MAX_ACCOUNTS {
        return Err(ParseError::InvalidAccount);
    }
    let mut roles = BTreeSet::new();
    let mut allowed_accounts = BTreeSet::new();
    for account in required {
        if !identifier(account.role) || !roles.insert(account.role) {
            return Err(ParseError::AccountMismatch);
        }
        context.account(account.address)?;
        allowed_accounts.insert(account.address.to_owned());
    }

    // A watch list built from required_accounts must contain every dependency.
    // Refuse undeclared reads even if a larger shared Bank contains that key.
    let scoped_context = ObservationContext {
        slot: context.slot,
        unix_seconds: context.unix_seconds,
        provider: context.provider,
        allowed_accounts: Some(allowed_accounts),
    };
    let values = adapter.read(&scoped_context)?;
    if values.is_empty() || values.len() > MAX_MEASUREMENTS {
        return Err(ParseError::InvalidValue);
    }
    let mut kinds = BTreeSet::new();
    if values.iter().any(|value| {
        !identifier(value.kind)
            || !kinds.insert(value.kind)
            || value.denominator == 0
            || value.updated_slot.is_some_and(|slot| slot > context.slot)
            || value
                .updated_unix_seconds
                .is_some_and(|time| time > context.unix_seconds)
    }) {
        return Err(ParseError::InvalidValue);
    }
    Ok(values)
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}
