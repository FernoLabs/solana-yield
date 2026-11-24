//! Shared SPL account and exact share-value checks.

use crate::{key, live::Measure, Account, ParseError};

pub(super) const SPL_TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

pub(super) fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("checked layout"))
}

pub(super) fn spl_account<'a>(account: &Account<'a>, len: usize) -> Result<&'a [u8], ParseError> {
    key(account.address)?;
    if account.owner != SPL_TOKEN || account.executable || account.lamports == 0 {
        return Err(ParseError::InvalidAccount);
    }
    if account.data.len() != len {
        return Err(ParseError::UnsupportedLayout);
    }
    Ok(account.data)
}

pub(super) fn mint_supply(account: &Account<'_>, authority: &str) -> Result<u64, ParseError> {
    let data = spl_account(account, 82)?;
    if data[45] != 1 || data[44] != 6 {
        return Err(ParseError::InvalidValue);
    }
    if data[..4] != 1_u32.to_le_bytes() || data[4..36] != key(authority)? {
        return Err(ParseError::AccountMismatch);
    }
    Ok(u64_at(data, 36))
}

pub(super) fn share_value(
    backing: u64,
    shares: u64,
    updated_unix_seconds: Option<u64>,
) -> Result<Vec<Measure>, ParseError> {
    if shares == 0 {
        return Err(ParseError::InvalidValue);
    }
    Ok(vec![Measure {
        kind: "share_value",
        numerator: u128::from(backing),
        denominator: u128::from(shares),
        updated_slot: None,
        updated_unix_seconds,
    }])
}
