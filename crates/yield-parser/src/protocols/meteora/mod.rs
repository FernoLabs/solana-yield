//! Unlocked Dynamic Vault assets per share at the same-Bank Clock.

mod adapter;
pub use adapter::Adapter;

use super::common::{mint_supply, share_value, u64_at};
use crate::{body, key, live::Measure, Account, ParseError};

const METEORA_PROGRAM: &str = "24Uqj9JCLxUeoC3hGfh5W3s9FM9uCHDS2SG3LYwBpyTi";
const METEORA_VAULT_TAG: [u8; 8] = [211, 8, 232, 43, 2, 152, 117, 119];
const METEORA_SERIALIZED_LEN: usize = 1_227;
const METEORA_ALLOCATED_LEN: usize = 1_232;
const LOCKED_PROFIT_SCALE: u128 = 1_000_000_000_000;

/// Calculate raw asset units per raw LP unit for a six-decimal Dynamic Vault.
/// The clock must come from the same verified Bank as both accounts.
pub fn observe_meteora(
    vault: &Account<'_>,
    share_mint: &Account<'_>,
    asset_mint: &str,
    clock_unix_seconds: u64,
) -> Result<Vec<Measure>, ParseError> {
    let size = vault.data.len();
    if !matches!(size, METEORA_SERIALIZED_LEN | METEORA_ALLOCATED_LEN) {
        return Err(ParseError::UnsupportedLayout);
    }
    let data = body(vault, METEORA_PROGRAM, size, METEORA_VAULT_TAG)?;
    let asset_key = key(asset_mint)?;
    let share_key = key(share_mint.address)?;
    if asset_key == [0; 32]
        || asset_key == share_key
        || data[83..115] != asset_key
        || data[115..147] != share_key
    {
        return Err(ParseError::AccountMismatch);
    }
    if data[8] > 1 {
        return Err(ParseError::InvalidValue);
    }

    let minted = mint_supply(share_mint, vault.address)?;
    // The first byte of the former last strategy word selects the supply rule.
    // Mainnet observes version 0 (mint supply) or 1 (vault-tracked supply).
    let shares = match data[1_075] {
        0 => minted,
        1 => {
            let tracked = u64_at(data, 1_076);
            if tracked < minted {
                return Err(ParseError::InvalidValue);
            }
            tracked
        }
        _ => return Err(ParseError::UnsupportedLayout),
    };
    let total = u64_at(data, 11);
    let reported_locked = u64_at(data, 1_203);
    let last_report = u64_at(data, 1_211);
    let degradation = u64_at(data, 1_219);
    if clock_unix_seconds == 0 {
        return Err(ParseError::InvalidValue);
    }
    let elapsed = clock_unix_seconds
        .checked_sub(last_report)
        .ok_or(ParseError::InvalidValue)?;
    let remaining_fraction =
        LOCKED_PROFIT_SCALE.saturating_sub(u128::from(elapsed) * u128::from(degradation));
    let still_locked =
        (u128::from(reported_locked) * remaining_fraction / LOCKED_PROFIT_SCALE) as u64;
    let unlocked = total
        .checked_sub(still_locked)
        .ok_or(ParseError::InvalidValue)?;
    share_value(unlocked, shares, (last_report != 0).then_some(last_report))
}

/// Standard account-reader entry point for this protocol.
pub use observe_meteora as observe;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::test_support::{account, put_u64};

    use crate::protocols::common::SPL_TOKEN;
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
    const METEORA_VAULT: &str = "5XCP3oD3JAuQyDpfBFFVUxsBxNjPQojpKuL4aVhHsDok";
    const METEORA_MINT: &str = "EZun6G5514FeqYtUv26cBHWLqXjAEdjGuoX6ThBpBtKj";

    fn meteora_fixture() -> ([u8; METEORA_ALLOCATED_LEN], [u8; 82]) {
        let mut vault = [0; METEORA_ALLOCATED_LEN];
        vault[..8].copy_from_slice(&METEORA_VAULT_TAG);
        vault[8] = 1;
        put_u64(&mut vault, 11, 122);
        vault[83..115].copy_from_slice(&key(USDT).unwrap());
        vault[115..147].copy_from_slice(&key(METEORA_MINT).unwrap());
        put_u64(&mut vault, 1_203, 21);
        put_u64(&mut vault, 1_211, 60);
        put_u64(&mut vault, 1_219, 500_000_000_000);
        let mut mint = [0; 82];
        mint[..4].copy_from_slice(&1_u32.to_le_bytes());
        mint[4..36].copy_from_slice(&key(METEORA_VAULT).unwrap());
        put_u64(&mut mint, 36, 90);
        mint[44] = 6;
        mint[45] = 1;
        (vault, mint)
    }

    #[test]
    fn meteora_time_unlocks_profit_and_tracked_supply_counts_external_burns() {
        let (mut vault, mint) = meteora_fixture();
        let mint = account(METEORA_MINT, SPL_TOKEN, &mint);
        let legacy = account(METEORA_VAULT, METEORA_PROGRAM, &vault);
        let reading = observe_meteora(&legacy, &mint, USDT, 61).unwrap();
        assert_eq!((reading[0].numerator, reading[0].denominator), (112, 90));
        assert_eq!(reading[0].updated_unix_seconds, Some(60));

        vault[1_075] = 1;
        put_u64(&mut vault, 1_076, 100);
        let tracked = account(METEORA_VAULT, METEORA_PROGRAM, &vault);
        let reading = observe_meteora(&tracked, &mint, USDT, 62).unwrap();
        assert_eq!((reading[0].numerator, reading[0].denominator), (122, 100));
    }

    #[test]
    fn meteora_refuses_future_reports_unknown_versions_and_foreign_mints() {
        let (mut vault, mint) = meteora_fixture();
        let mint = account(METEORA_MINT, SPL_TOKEN, &mint);
        let expected = account(METEORA_VAULT, METEORA_PROGRAM, &vault);
        assert!(observe_meteora(&expected, &mint, USDT, 59).is_err());
        assert!(observe_meteora(
            &expected,
            &Account {
                address: USDC,
                ..mint
            },
            USDT,
            61
        )
        .is_err());
        vault[1_075] = 1;
        put_u64(&mut vault, 1_076, 89);
        assert!(observe_meteora(
            &account(METEORA_VAULT, METEORA_PROGRAM, &vault),
            &mint,
            USDT,
            61
        )
        .is_err());
        vault[1_075] = 2;
        assert!(matches!(
            observe_meteora(
                &account(METEORA_VAULT, METEORA_PROGRAM, &vault),
                &mint,
                USDT,
                61
            ),
            Err(ParseError::UnsupportedLayout)
        ));
    }
}
