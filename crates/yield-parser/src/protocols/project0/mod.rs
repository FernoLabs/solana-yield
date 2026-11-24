//! Stored share value for native Project 0 lending banks.
//!
//! Layout: Marginfi Bank and BankConfig at revision
//! 8665b8d1afc4d39bf7e575a85c663580e35c6314.
//! <https://github.com/0dotxyz/marginfi-v2/blob/8665b8d1afc4d39bf7e575a85c663580e35c6314/type-crate/src/types/bank.rs>
//! <https://github.com/0dotxyz/marginfi-v2/blob/8665b8d1afc4d39bf7e575a85c663580e35c6314/type-crate/src/types/bank_config.rs>

mod adapter;
pub use adapter::Adapter;

use crate::{body, key, live::Measure, Account, ParseError};

pub const PROGRAM: &str = "MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA";
const DISCRIMINATOR: [u8; 8] = [142, 49, 166, 242, 50, 66, 97, 188];
const BANK_V1_SIZE: usize = 8 + 1_856;
const BANK_V2_SIZE: usize = 8 + 3_904;
const I80F48_SCALE: u128 = 1 << 48;
const ASSET_TAG_OFFSET: usize = 785;

/// Reads the bank's stored native asset units per asset share. The bank's
/// last interest-accrual time is distinct from the source Bank's time.
/// Wrapped positions require their integrated protocol's valuation.
pub fn observe(
    bank: &Account<'_>,
    group: &str,
    asset_mint: &str,
) -> Result<Vec<Measure>, ParseError> {
    let size = match bank.data.len() {
        BANK_V1_SIZE => BANK_V1_SIZE,
        BANK_V2_SIZE => BANK_V2_SIZE,
        _ => return Err(ParseError::UnsupportedLayout),
    };
    let data = body(bank, PROGRAM, size, DISCRIMINATOR)?;
    let expected_group = key(group)?;
    let expected_mint = key(asset_mint)?;
    if [expected_group, expected_mint].contains(&[0; 32])
        || data[8..40] != expected_mint
        || data[41..73] != expected_group
    {
        return Err(ParseError::AccountMismatch);
    }
    // The known native tags are default (0) and SOL (1). Staked assets and
    // lending integrations have different balance and valuation rules.
    if !matches!(data[ASSET_TAG_OFFSET], 0 | 1) || data[40] >= 24 {
        return Err(ParseError::UnsupportedLayout);
    }
    let share = i128::from_le_bytes(data[80..96].try_into().expect("validated bank size"));
    let share = u128::try_from(share).map_err(|_| ParseError::InvalidValue)?;
    if share == 0 {
        return Err(ParseError::InvalidValue);
    }
    let accrual = i64::from_le_bytes(data[288..296].try_into().expect("validated bank size"));
    let accrual = u64::try_from(accrual).map_err(|_| ParseError::InvalidValue)?;
    Ok(vec![Measure {
        kind: "stored_asset_share_value",
        numerator: share,
        denominator: I80F48_SCALE,
        updated_slot: None,
        updated_unix_seconds: (accrual != 0).then_some(accrual),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANK: &str = "2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB";
    const GROUP: &str = "4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8";
    const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const SHARE_BITS: i128 = 354_956_980_378_147;
    const ACCRUAL_TIME: i64 = 1_795_389_000;

    fn fixture(size: usize) -> Vec<u8> {
        let mut data = vec![0; size];
        data[..8].copy_from_slice(&DISCRIMINATOR);
        data[8..40].copy_from_slice(&key(MINT).unwrap());
        data[40] = 6;
        data[41..73].copy_from_slice(&key(GROUP).unwrap());
        data[80..96].copy_from_slice(&SHARE_BITS.to_le_bytes());
        data[288..296].copy_from_slice(&ACCRUAL_TIME.to_le_bytes());
        data
    }

    fn account(data: &[u8]) -> Account<'_> {
        Account {
            address: BANK,
            owner: PROGRAM,
            executable: false,
            lamports: 1,
            data,
        }
    }

    #[test]
    fn reads_the_exact_ratio_and_original_accrual_time_in_both_layouts() {
        for size in [BANK_V1_SIZE, BANK_V2_SIZE] {
            let data = fixture(size);
            let result = observe(&account(&data), GROUP, MINT).unwrap();
            assert_eq!(result.len(), 1);
            let measure = &result[0];
            assert_eq!(measure.kind, "stored_asset_share_value");
            assert_eq!(measure.numerator, SHARE_BITS as u128);
            assert_eq!(measure.denominator, 1 << 48);
            assert_eq!(measure.updated_slot, None);
            assert_eq!(measure.updated_unix_seconds, Some(ACCRUAL_TIME as u64));
        }
    }

    #[test]
    fn rejects_wrapped_and_unknown_bank_tags() {
        let mut data = fixture(BANK_V2_SIZE);
        for tag in [2, 3, 4, 5, 6, 255] {
            data[ASSET_TAG_OFFSET] = tag;
            assert!(matches!(
                observe(&account(&data), GROUP, MINT),
                Err(ParseError::UnsupportedLayout)
            ));
        }
    }

    #[test]
    fn checks_owner_discriminator_layout_group_and_mint() {
        let mut data = fixture(BANK_V1_SIZE);
        let mut wrong_owner = account(&data);
        wrong_owner.owner = MINT;
        assert!(matches!(
            observe(&wrong_owner, GROUP, MINT),
            Err(ParseError::InvalidAccount)
        ));
        assert!(matches!(
            observe(&account(&data), MINT, GROUP),
            Err(ParseError::AccountMismatch)
        ));
        assert!(matches!(
            observe(&account(&data[..BANK_V1_SIZE - 1]), GROUP, MINT),
            Err(ParseError::UnsupportedLayout)
        ));
        data[0] ^= 1;
        assert!(matches!(
            observe(&account(&data), GROUP, MINT),
            Err(ParseError::UnsupportedLayout)
        ));
    }

    #[test]
    fn rejects_negative_values_and_retains_missing_accrual_time() {
        let mut data = fixture(BANK_V2_SIZE);
        data[288..296].fill(0);
        assert_eq!(
            observe(&account(&data), GROUP, MINT).unwrap()[0].updated_unix_seconds,
            None
        );
        data[80..96].copy_from_slice(&(-1_i128).to_le_bytes());
        assert!(matches!(
            observe(&account(&data), GROUP, MINT),
            Err(ParseError::InvalidValue)
        ));
    }
}
