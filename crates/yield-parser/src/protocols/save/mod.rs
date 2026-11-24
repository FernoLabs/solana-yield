//! Save mainnet reserve observations.
//!
//! The packed v1 layout and net-assets calculation follow the pinned
//! [mainnet SDK](https://github.com/solendprotocol/solana-program-library/blob/d04ce00bbf4356c4fd32b3be38eb9760b696bb3e/token-lending/sdk/src/state/reserve.rs).

mod adapter;
pub use adapter::Adapter;

use crate::{key, live::Measure, Account, ParseError};

pub const PROGRAM: &str = "So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo";
const SIZE: usize = 619;
const WAD: u128 = 1_000_000_000_000_000_000;

/// Return the stored net assets per outstanding collateral share in raw
/// token units. The observation carries the reserve's last refresh slot.
/// An instruction that changes balances can mark the reserve stale. The
/// stored assets are still observable, with accrual after that slot omitted.
pub fn observe(
    reserve: &Account<'_>,
    lending_market: &str,
    asset_mint: &str,
) -> Result<Vec<Measure>, ParseError> {
    key(reserve.address)?;
    if reserve.owner != PROGRAM || reserve.executable || reserve.lamports == 0 {
        return Err(ParseError::InvalidAccount);
    }

    let data: &[u8; SIZE] = reserve
        .data
        .try_into()
        .map_err(|_| ParseError::UnsupportedLayout)?;
    if data[0] != 1 || data[9] > 1 {
        return Err(ParseError::UnsupportedLayout);
    }
    let expected_market = key(lending_market)?;
    let expected_asset = key(asset_mint)?;
    if expected_market == [0; 32]
        || expected_asset == [0; 32]
        || data[10..42] != expected_market
        || data[42..74] != expected_asset
        || data[227..259] == expected_asset
        || data[227..259] == [0; 32]
    {
        return Err(ParseError::AccountMismatch);
    }

    let slot = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let available = u64::from_le_bytes(data[171..179].try_into().unwrap());
    let borrowed_wads = u128::from_le_bytes(data[179..195].try_into().unwrap());
    let outstanding = u64::from_le_bytes(data[259..267].try_into().unwrap());
    let protocol_fees_wads = u128::from_le_bytes(data[373..389].try_into().unwrap());
    if slot == 0 || outstanding == 0 || data[74] > 18 {
        return Err(ParseError::InvalidValue);
    }

    let cash_wads = u128::from(available) * WAD;
    let net_assets_wads = if borrowed_wads >= protocol_fees_wads {
        cash_wads.checked_add(borrowed_wads - protocol_fees_wads)
    } else {
        cash_wads.checked_sub(protocol_fees_wads - borrowed_wads)
    }
    .ok_or(ParseError::InvalidValue)?;
    Ok(vec![Measure {
        kind: "stored_assets_per_share",
        numerator: net_assets_wads,
        denominator: u128::from(outstanding) * WAD,
        updated_slot: Some(slot),
        updated_unix_seconds: None,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_RESERVE: &str = "BJfY2E6TVEQe896kfR1AhhYCECVKfbceGg2ASU2LXdiA";
    const TEST_MARKET: &str = "7XttJ7hp83u5euzT7ybC5zsjdgKA4WPbQHVS27CATAJH";
    const TEST_ASSET: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const TEST_COLLATERAL: &str = "927hg5BZTEK2LdtrTu9e8RCMm3QBCfNrcdDDgvVykcCX";

    fn reserve(data: &[u8]) -> Account<'_> {
        Account {
            address: TEST_RESERVE,
            owner: PROGRAM,
            executable: false,
            lamports: 1,
            data,
        }
    }

    // These fields come from a public Save reserve record for the JLP/USDC
    // market, encoded at the offsets of the pinned SDK.
    fn fixture() -> [u8; SIZE] {
        let mut data = [0; SIZE];
        data[0] = 1;
        data[1..9].copy_from_slice(&472_566_470_u64.to_le_bytes());
        data[9] = 1;
        data[10..42].copy_from_slice(&key(TEST_MARKET).unwrap());
        data[42..74].copy_from_slice(&key(TEST_ASSET).unwrap());
        data[74] = 6;
        data[171..179].copy_from_slice(&28_778_745_508_u64.to_le_bytes());
        data[179..195].copy_from_slice(&24_204_413_866_767_537_538_847_635_422_u128.to_le_bytes());
        data[227..259].copy_from_slice(&key(TEST_COLLATERAL).unwrap());
        data[259..267].copy_from_slice(&47_580_253_020_u64.to_le_bytes());
        data[373..389].copy_from_slice(&602_913_312_703_429_692_u128.to_le_bytes());
        data
    }

    #[test]
    fn observes_net_assets_with_protocol_fees_removed_at_the_stored_slot() {
        let values = observe(&reserve(&fixture()), TEST_MARKET, TEST_ASSET).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].kind, "stored_assets_per_share");
        assert_eq!(values[0].numerator, 52_983_159_374_164_624_226_144_205_730);
        assert_eq!(values[0].denominator, 47_580_253_020 * WAD);
        assert_eq!(values[0].updated_slot, Some(472_566_470));
        assert_eq!(values[0].updated_unix_seconds, None);
    }

    #[test]
    fn rejects_the_wrong_owner_layout_and_reserve_binding() {
        let mut data = fixture();
        let wrong_owner = Account {
            owner: TEST_MARKET,
            ..reserve(&data)
        };
        assert!(matches!(
            observe(&wrong_owner, TEST_MARKET, TEST_ASSET),
            Err(ParseError::InvalidAccount)
        ));
        assert!(matches!(
            observe(&reserve(&data[..SIZE - 1]), TEST_MARKET, TEST_ASSET),
            Err(ParseError::UnsupportedLayout)
        ));
        assert!(matches!(
            observe(&reserve(&data), TEST_MARKET, TEST_COLLATERAL),
            Err(ParseError::AccountMismatch)
        ));
        data[0] = 2;
        assert!(matches!(
            observe(&reserve(&data), TEST_MARKET, TEST_ASSET),
            Err(ParseError::UnsupportedLayout)
        ));
    }

    #[test]
    fn rejects_empty_supply_and_impossible_fees() {
        let mut data = fixture();
        data[259..267].fill(0);
        assert!(matches!(
            observe(&reserve(&data), TEST_MARKET, TEST_ASSET),
            Err(ParseError::InvalidValue)
        ));
        let mut data = fixture();
        data[373..389].copy_from_slice(&u128::MAX.to_le_bytes());
        assert!(matches!(
            observe(&reserve(&data), TEST_MARKET, TEST_ASSET),
            Err(ParseError::InvalidValue)
        ));
    }

    #[test]
    fn preserves_zero_and_large_valid_net_balances() {
        let mut data = fixture();
        data[171..179].copy_from_slice(&1_u64.to_le_bytes());
        data[179..195].copy_from_slice(&u128::MAX.to_le_bytes());
        data[373..389].copy_from_slice(&WAD.to_le_bytes());
        let values = observe(&reserve(&data), TEST_MARKET, TEST_ASSET).unwrap();
        assert_eq!(values[0].numerator, u128::MAX);

        data[179..195].fill(0);
        let values = observe(&reserve(&data), TEST_MARKET, TEST_ASSET).unwrap();
        assert_eq!(values[0].numerator, 0);
    }
}
