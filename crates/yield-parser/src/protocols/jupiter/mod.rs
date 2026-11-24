//! Jupiter Lend Earn pool reader for the main market.
//!
//! Layout source: Jupiter Lend 0.1.9
//! <https://github.com/jup-ag/jupiter-lend/blob/7960a14a5a760abc41db3ed1395dc5cb56a1f8db/target/idl/lending.json>.
//! Conversion scale and update rules:
//! <https://github.com/Instadapp/fluid-solana-programs/blob/626b177f235224bc5d074d39439cd2558f542886/programs/lending/src/utils/helpers.rs>.

mod adapter;
pub use adapter::Adapter;

pub mod ledger;

use crate::{body, key, Account, ParseError, ShareValue};

const PROGRAM: &str = "jup3YeL8QhtSx1e253b2FDvsMNC87fDrgQZivbrndc9";
const LENDING_SIZE: usize = 196;
const LENDING_DISCRIMINATOR: [u8; 8] = [135, 199, 82, 16, 249, 131, 182, 241];
const EXCHANGE_PRICE_SCALE: u64 = 1_000_000_000_000;

/// Decode the stored receipt-token exchange price in raw asset units per raw share.
///
/// The pool updates this price as interest and any embedded rewards accrue.
/// This value describes the stored state. A fresh redemption quote can apply
/// further accrual, and an annualized return needs more than one observation.
pub fn share_value(
    account: &Account<'_>,
    liquidity_reserve: &str,
    asset_mint: &str,
    share_mint: &str,
) -> Result<ShareValue, ParseError> {
    let data = body(account, PROGRAM, LENDING_SIZE, LENDING_DISCRIMINATOR)?;
    let asset = key(asset_mint)?;
    let share = key(share_mint)?;
    let reserve = key(liquidity_reserve)?;
    if [asset, share, reserve].contains(&[0; 32])
        || asset == share
        || data[8..40] != asset
        || data[40..72] != share
        || data[131..163] != reserve
        || data[163..195] == [0; 32]
    {
        return Err(ParseError::AccountMismatch);
    }

    // The exact layout check above establishes that the fixed offsets are in bounds.
    let stored_liquidity_price = u64::from_le_bytes(data[107..115].try_into().unwrap());
    let stored_share_price = u64::from_le_bytes(data[115..123].try_into().unwrap());
    let last_update = u64::from_le_bytes(data[123..131].try_into().unwrap());
    if stored_liquidity_price == 0
        || stored_share_price < EXCHANGE_PRICE_SCALE
        || last_update == 0
        || last_update > i64::MAX as u64
    {
        return Err(ParseError::InvalidValue);
    }

    ShareValue::new(
        u128::from(stored_share_price),
        u128::from(EXCHANGE_PRICE_SCALE),
        None,
        Some(last_update),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Public mainnet Jupiter Earn USDC Lending account at slot 463283500.
    // SHA-256 of the 196 account-data bytes:
    // 23951a243af8877488481c9923a716dc8fdbb9e72d71f79b39cfc181869438f0
    const SNAPSHOT: &[u8; LENDING_SIZE] =
        include_bytes!("../../../fixtures/jupiter-earn-usdc-lending.bin");
    const ADDRESS: &str = "2vVYHYM8VYnvZqQWpTJSj8o8DBf1wM8pVs3bsTgYZiqJ";
    const RESERVE: &str = "94vK29npVbyRHXH63rRcTiSr26SFhrQTzbpNJuhQEDu";
    const MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const SHARE: &str = "9BEcn9aPEmhSPbPQeFGjidRiEKki46fVQDyPpSQXPA2D";

    fn account(data: &[u8]) -> Account<'_> {
        Account {
            address: ADDRESS,
            owner: PROGRAM,
            executable: false,
            lamports: 1,
            data,
        }
    }

    fn parse(data: &[u8]) -> Result<ShareValue, ParseError> {
        share_value(&account(data), RESERVE, MINT, SHARE)
    }

    #[test]
    fn returns_the_stored_receipt_price_from_the_usdc_pool() {
        assert_eq!(
            parse(SNAPSHOT).unwrap(),
            ShareValue::new(
                1_067_222_452_096,
                1_000_000_000_000,
                None,
                Some(1_793_362_942)
            )
            .unwrap()
        );
    }

    #[test]
    fn checks_the_reserve_and_both_mint_bindings() {
        for offset in [8, 40, 131] {
            let mut data = *SNAPSHOT;
            data[offset] ^= 1;
            assert!(matches!(parse(&data), Err(ParseError::AccountMismatch)));
        }
    }

    #[test]
    fn rejects_wrong_owner_and_layout() {
        let mut wrong_owner = account(SNAPSHOT);
        wrong_owner.owner = MINT;
        assert!(share_value(&wrong_owner, RESERVE, MINT, SHARE).is_err());
        assert!(parse(&SNAPSHOT[1..]).is_err());
        let mut data = *SNAPSHOT;
        data[0] ^= 1;
        assert!(parse(&data).is_err());
    }

    #[test]
    fn rejects_an_uninitialized_price_or_update_time() {
        for offset in [107, 115, 123] {
            let mut data = *SNAPSHOT;
            data[offset..offset + 8].fill(0);
            assert!(matches!(parse(&data), Err(ParseError::InvalidValue)));
        }
    }
}
