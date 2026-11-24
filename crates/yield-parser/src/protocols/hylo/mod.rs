//! Gross hyUSD value per eHYUSD share from same-Bank account data.

use super::common::{mint_supply, share_value, spl_account, u64_at};
use crate::{body, key, live::Measure, Account, ParseError};

const HYLO_PROGRAM: &str = "HysTabVUfmQBFcmzu1ctRd1Y1fxd66RBpboy1bmtDSQQ";
const HYLO_CONFIG: &str = "2jk7miWrsTbt5hUSaCXPkEQPvuUMgbFLpgMzMQw3Z6ar";
const HYLO_CONFIG_TAG: [u8; 8] = [26, 108, 14, 123, 116, 230, 129, 43];
const HYLO_POOL: &str = "EqozKyMj7FVnLHc2cJj3VC25aBr4AhVh1cGM2WDajGe9";
const HYLO_POOL_AUTHORITY: &str = "5YrRAQag9BbJkauDtJkd1vsTquXT6N46oU8rJ66GDxHd";
const HYLO_ASSET_MINT: &str = "5YMkXAYccHSGnHn9nob9xEvv6Pvka9DZWH7nTbotTu9E";
const HYLO_SHARE_MINT: &str = "HnnGv3HrSqjRpgdFmx7vQGjntNEoex1SU4e9Lxcxuihz";
const HYLO_MINT_AUTHORITY: &str = "5YWerkcqAXTSCzKC1X52BXtfv2aoNCB6wzv7wEXuGWpq";

/// Read gross pool hyUSD per eHYUSD share. Both mints use six decimals.
pub fn observe_hylo(
    config: &Account<'_>,
    pool: &Account<'_>,
    share_mint: &Account<'_>,
) -> Result<Vec<Measure>, ParseError> {
    if (config.address, pool.address, share_mint.address)
        != (HYLO_CONFIG, HYLO_POOL, HYLO_SHARE_MINT)
    {
        return Err(ParseError::AccountMismatch);
    }
    let config_data = body(config, HYLO_PROGRAM, 107, HYLO_CONFIG_TAG)?;
    let withdrawal_fee = u64_at(config_data, 43);
    if config_data[52] > 1
        || withdrawal_fee > 10_000
        || (withdrawal_fee != 0 && config_data[51] != (-4_i8) as u8)
        || [53, 62, 79].into_iter().any(|offset| {
            u64_at(config_data, offset) != 0 && config_data[offset + 8] != (-6_i8) as u8
        })
    {
        return Err(ParseError::InvalidValue);
    }
    let pool_data = spl_account(pool, 165)?;
    if pool_data[108] != 1
        || pool_data[..32] != key(HYLO_ASSET_MINT)?
        || pool_data[32..64] != key(HYLO_POOL_AUTHORITY)?
    {
        return Err(ParseError::AccountMismatch);
    }

    let backing = u64_at(pool_data, 64);
    let shares = mint_supply(share_mint, HYLO_MINT_AUTHORITY)?;
    share_value(backing, shares, None)
}

/// Standard account-reader entry point for this protocol.
pub use observe_hylo as observe;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::test_support::account;

    use crate::protocols::common::SPL_TOKEN;
    const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";

    #[test]
    fn hylo_authenticates_mainnet_custody_and_uses_exact_units() {
        let config = account(
            HYLO_CONFIG,
            HYLO_PROGRAM,
            include_bytes!("../../../fixtures/live-hylo-pool-config.bin"),
        );
        let pool = account(
            HYLO_POOL,
            SPL_TOKEN,
            include_bytes!("../../../fixtures/live-hylo-hyusd-pool.bin"),
        );
        let mint = account(
            HYLO_SHARE_MINT,
            SPL_TOKEN,
            include_bytes!("../../../fixtures/live-hylo-ehyusd-mint.bin"),
        );
        let reading = observe_hylo(&config, &pool, &mint).unwrap();
        assert_eq!(reading[0].kind, "share_value");
        assert_eq!(
            (reading[0].numerator, reading[0].denominator),
            (19_012_166_440_522, 12_773_576_779_231)
        );
        assert!(observe_hylo(
            &config,
            &Account {
                address: USDT,
                ..pool
            },
            &mint
        )
        .is_err());
        assert!(observe_hylo(
            &config,
            &pool,
            &Account {
                owner: HYLO_PROGRAM,
                ..mint
            }
        )
        .is_err());
    }
}
