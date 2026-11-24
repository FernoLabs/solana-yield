//! Posted simple annual rates from Elemental deposit pools.

mod adapter;
pub use adapter::Adapter;

use super::common::u64_at;
use crate::{body, key, live::Measure, Account, ParseError};

const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
const ELEMENTAL_V1: &str = "ELE5vYY81W7UCpTPs7SyD6Bwm5FwZBntTW8PiGqM5d4A";
const ELEMENTAL_V2: &str = "ELE7vRKKCzWT5SEt1eey2Snn3z6pWmBuv5AjDq3BeS1Y";
const ELEMENTAL_POOL_TAG: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];

/// Read the fixed deposit's posted annual simple rate in 1e-9 units.
pub fn observe_elemental(pool: &Account<'_>, asset_mint: &str) -> Result<Vec<Measure>, ParseError> {
    let (size, extra_key_bytes) = match pool.owner {
        ELEMENTAL_V1 => (254, 0),
        ELEMENTAL_V2 => (415, 32),
        _ => return Err(ParseError::InvalidAccount),
    };
    let data = body(pool, pool.owner, size, ELEMENTAL_POOL_TAG)?;
    if !matches!(asset_mint, USDC | USDT) || data[8..40] != key(asset_mint)? {
        return Err(ParseError::AccountMismatch);
    }

    // V2 inserts one public key before the amount fields.
    let unit = u64_at(data, 72 + extra_key_bytes);
    let cap = u64_at(data, 96 + extra_key_bytes);
    let deposits = u64_at(data, 104 + extra_key_bytes);
    let rate = u64_at(data, 128 + extra_key_bytes);
    if unit != 1_000_000 || cap == 0 || deposits > cap {
        return Err(ParseError::InvalidValue);
    }
    Ok(vec![Measure {
        kind: "posted_apr",
        numerator: u128::from(rate),
        denominator: 1_000_000_000,
        updated_slot: None,
        updated_unix_seconds: None,
    }])
}

/// Standard account-reader entry point for this protocol.
pub use observe_elemental as observe;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::test_support::{account, put_u64};

    const METEORA_VAULT: &str = "5XCP3oD3JAuQyDpfBFFVUxsBxNjPQojpKuL4aVhHsDok";

    #[test]
    fn elemental_versions_use_posted_apr_and_bind_the_asset() {
        for (program, size, extra_key_bytes) in [(ELEMENTAL_V1, 254, 0), (ELEMENTAL_V2, 415, 32)] {
            let mut data = vec![0; size];
            data[..8].copy_from_slice(&ELEMENTAL_POOL_TAG);
            data[8..40].copy_from_slice(&key(USDT).unwrap());
            put_u64(&mut data, 72 + extra_key_bytes, 1_000_000);
            put_u64(&mut data, 96 + extra_key_bytes, 41_000_000);
            put_u64(&mut data, 104 + extra_key_bytes, 40_000_000);
            put_u64(&mut data, 128 + extra_key_bytes, 93_000_000);
            let pool = account(METEORA_VAULT, program, &data);
            let reading = observe_elemental(&pool, USDT).unwrap();
            assert_eq!(reading[0].kind, "posted_apr");
            assert_eq!(
                (reading[0].numerator, reading[0].denominator),
                (93_000_000, 1_000_000_000)
            );
            assert!(observe_elemental(&pool, USDC).is_err());
            put_u64(&mut data, 104 + extra_key_bytes, 42_000_000);
            assert!(observe_elemental(&account(METEORA_VAULT, program, &data), USDT).is_err());
        }
    }
}
