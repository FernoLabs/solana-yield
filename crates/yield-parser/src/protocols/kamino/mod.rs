//! Stored Kamino Lending Reserve v1 receipt value.

pub mod ledger;

use crate::{body, key, Account, ParseError, ShareValue};

pub const PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const DISCRIMINATOR: [u8; 8] = [43, 242, 204, 202, 26, 247, 59, 127];
const RESERVE_SIZE: usize = 8_624;
const FRACTION_SCALE: u128 = 1 << 60;

/// Raw asset units per raw collateral receipt unit at the reserve's stored
/// state. Accrual enters this value when Kamino updates the reserve.
///
/// See [Kamino's reserve accounting][reserve] for total supply and the inverse
/// collateral exchange rate. The ratio keeps the full stored Q60 asset value.
///
/// [reserve]: https://github.com/Kamino-Finance/klend/blob/686913ee421e0c5f3dc44c4c8344461ee0e4fb89/programs/klend/src/state/reserve.rs
pub fn share_value(
    account: &Account<'_>,
    market: &str,
    asset_mint: &str,
    share_mint: &str,
) -> Result<ShareValue, ParseError> {
    let data = body(account, PROGRAM, RESERVE_SIZE, DISCRIMINATOR)?;
    if u64_at(data, 8) != 1 || data[4_856] > 2 {
        return Err(ParseError::UnsupportedLayout);
    }

    let market = key(market)?;
    let asset = key(asset_mint)?;
    let share = key(share_mint)?;
    if [market, asset, share].contains(&[0; 32])
        || asset == share
        || data[32..64] != market
        || data[128..160] != asset
        || data[2_560..2_592] != share
    {
        return Err(ParseError::AccountMismatch);
    }

    let available = u128::from(u64_at(data, 224)) * FRACTION_SCALE;
    let borrowed = u128_at(data, 232);
    let protocol_fees = u128_at(data, 344);
    let referrer_fees = u128_at(data, 360);
    let pending_referrer_fees = u128_at(data, 376);
    let net_assets = available
        .checked_add(borrowed)
        .and_then(|gross| gross.checked_sub(protocol_fees))
        .and_then(|net| net.checked_sub(referrer_fees))
        .and_then(|net| net.checked_sub(pending_referrer_fees))
        .ok_or(ParseError::InvalidValue)?;
    let receipt_supply = u128::from(u64_at(data, 2_592)) * FRACTION_SCALE;
    // Kamino's LastUpdate stores slot and Unix time. A zero field has no time evidence.
    let slot = u64_at(data, 16);
    let timestamp = u64::from(u32::from_le_bytes(
        data[28..32].try_into().expect("validated reserve size"),
    ));
    ShareValue::new(
        net_assets,
        receipt_supply,
        Some(slot).filter(|slot| *slot > 0),
        Some(timestamp).filter(|timestamp| *timestamp > 0),
    )
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .expect("validated reserve size"),
    )
}

fn u128_at(data: &[u8], offset: usize) -> u128 {
    u128::from_le_bytes(
        data[offset..offset + 16]
            .try_into()
            .expect("validated reserve size"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp";
    const MARKET: &str = "7u3HeHxYDLhnCoErrtycNokbQYbWGzLs6JSDqGAv5PfF";
    const ASSET: &str = "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH";
    const SHARE: &str = "BG6gsv8goyoJguEbLquUZFNiZ8aGTXgo4DyH9h8z9qao";

    fn fixture() -> [u8; RESERVE_SIZE] {
        // Project the fields of the public USDG reserve capture in the
        // yield-indexer native fixtures, leaving unrelated fields zeroed.
        let mut data = [0; RESERVE_SIZE];
        data[..8].copy_from_slice(&DISCRIMINATOR);
        data[8..16].copy_from_slice(&1_u64.to_le_bytes());
        data[16..24].copy_from_slice(&452_570_160_u64.to_le_bytes());
        data[28..32].copy_from_slice(&1_790_933_433_u32.to_le_bytes());
        data[32..64].copy_from_slice(&key(MARKET).unwrap());
        data[128..160].copy_from_slice(&key(ASSET).unwrap());
        data[2_560..2_592].copy_from_slice(&key(SHARE).unwrap());
        data[224..232].copy_from_slice(&6_895_917_337_162_u64.to_le_bytes());
        data[232..248]
            .copy_from_slice(&43_118_557_849_404_336_165_571_163_696_725_u128.to_le_bytes());
        data[344..360].copy_from_slice(&89_435_828_376_432_929_352_355_900_001_u128.to_le_bytes());
        data[2_592..2_600].copy_from_slice(&40_598_199_076_642_u64.to_le_bytes());
        data
    }

    fn account(data: &[u8]) -> Account<'_> {
        Account {
            address: ADDRESS,
            owner: PROGRAM,
            executable: false,
            lamports: 1,
            data,
        }
    }

    #[test]
    fn values_the_stored_usdg_reserve_at_full_precision() {
        let data = fixture();
        let value = share_value(&account(&data), MARKET, ASSET, SHARE).unwrap();
        let expected = ShareValue::new(
            50_979_573_413_033_157_950_669_539_918_836,
            46_806_536_763_770_400_250_274_189_934_592,
            Some(452_570_160),
            Some(1_790_933_433),
        )
        .unwrap();
        assert_eq!(value, expected);
    }

    #[test]
    fn subtracts_each_fee_without_rounding_net_assets() {
        let mut data = fixture();
        data[224..232].copy_from_slice(&9_u64.to_le_bytes());
        data[232..248].copy_from_slice(&(FRACTION_SCALE * 5 / 2).to_le_bytes());
        data[344..360].copy_from_slice(&(FRACTION_SCALE * 9 / 8).to_le_bytes());
        data[360..376].copy_from_slice(&(FRACTION_SCALE / 4).to_le_bytes());
        data[376..392].copy_from_slice(&(FRACTION_SCALE * 3 / 8).to_le_bytes());
        data[2_592..2_600].copy_from_slice(&8_u64.to_le_bytes());
        let value = share_value(&account(&data), MARKET, ASSET, SHARE).unwrap();
        assert_eq!(value.numerator(), FRACTION_SCALE * 39 / 4);
        assert_eq!(value.denominator(), FRACTION_SCALE * 8);
    }

    #[test]
    fn verifies_owner_layout_and_mint_bindings() {
        let mut data = fixture();
        let wrong_owner = Account {
            owner: MARKET,
            ..account(&data)
        };
        assert!(share_value(&wrong_owner, MARKET, ASSET, SHARE).is_err());
        assert!(share_value(&account(&data[1..]), MARKET, ASSET, SHARE).is_err());
        for (market, asset, share) in [
            (ASSET, ASSET, SHARE),
            (MARKET, SHARE, SHARE),
            (MARKET, ASSET, MARKET),
        ] {
            assert!(matches!(
                share_value(&account(&data), market, asset, share),
                Err(ParseError::AccountMismatch),
            ));
        }
        data[8] = 2;
        assert!(matches!(
            share_value(&account(&data), MARKET, ASSET, SHARE),
            Err(ParseError::UnsupportedLayout),
        ));
        data[8] = 1;
        data[4_856] = 2;
        assert!(share_value(&account(&data), MARKET, ASSET, SHARE).is_ok());
        data[4_856] = 3;
        assert!(matches!(
            share_value(&account(&data), MARKET, ASSET, SHARE),
            Err(ParseError::UnsupportedLayout),
        ));
    }

    #[test]
    fn rejects_missing_shares_fee_underflow_and_asset_overflow() {
        let mut data = fixture();
        data[2_592..2_600].fill(0);
        assert!(share_value(&account(&data), MARKET, ASSET, SHARE).is_err());
        let mut data = fixture();
        data[360..376].copy_from_slice(&u128::MAX.to_le_bytes());
        assert!(share_value(&account(&data), MARKET, ASSET, SHARE).is_err());
        let mut data = fixture();
        data[232..248].copy_from_slice(&u128::MAX.to_le_bytes());
        assert!(share_value(&account(&data), MARKET, ASSET, SHARE).is_err());
    }
}
