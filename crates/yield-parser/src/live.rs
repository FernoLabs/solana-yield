//! Measurements from verified Solana account state.

use crate::{Account, ParseError};

pub const CLOCK: &str = "SysvarC1ock11111111111111111111111111111111";
const SYSVAR: &str = "Sysvar1111111111111111111111111111111111111";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measure {
    pub kind: &'static str,
    pub numerator: u128,
    pub denominator: u128,
    pub updated_slot: Option<u64>,
    pub updated_unix_seconds: Option<u64>,
}

/// Validate Clock against the account batch's exact Bank slot.
pub fn clock_unix_seconds(account: &Account<'_>, slot: u64) -> Result<u64, ParseError> {
    if account.address != CLOCK
        || account.owner != SYSVAR
        || account.executable
        || account.lamports == 0
    {
        return Err(ParseError::InvalidAccount);
    }
    let data: &[u8; 40] = account
        .data
        .try_into()
        .map_err(|_| ParseError::UnsupportedLayout)?;
    if u64::from_le_bytes(data[..8].try_into().unwrap()) != slot {
        return Err(ParseError::AccountMismatch);
    }
    let time = i64::from_le_bytes(data[32..].try_into().unwrap());
    u64::try_from(time)
        .ok()
        .filter(|time| *time > 0)
        .ok_or(ParseError::InvalidValue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_must_come_from_the_expected_bank() {
        let mut data = [0_u8; 40];
        data[..8].copy_from_slice(&100_u64.to_le_bytes());
        data[32..].copy_from_slice(&1_700_000_000_i64.to_le_bytes());
        let clock = Account {
            address: CLOCK,
            owner: SYSVAR,
            executable: false,
            lamports: 1,
            data: &data,
        };
        assert_eq!(clock_unix_seconds(&clock, 100), Ok(1_700_000_000));
        assert_eq!(
            clock_unix_seconds(&clock, 101),
            Err(ParseError::AccountMismatch)
        );
    }
}
