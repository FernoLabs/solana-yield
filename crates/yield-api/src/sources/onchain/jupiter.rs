//! Jupiter liquidity reserve + rate-model v2. Amounts remain in the mint's raw units.

use anyhow::{anyhow, ensure, Result};

use super::{authenticate, curve_apr, key, u16_at, u64_at, NativeRate};
use crate::rpc::{Account, RpcSnapshot};

pub(super) const PROGRAM: &str = "jupeiUmn818Jg1ekPURTpr4mFo29p46vygyykFJ3wZC";
const RESERVE_DISCRIMINATOR: [u8; 8] = [21, 18, 59, 135, 120, 20, 31, 12];
const MODEL_DISCRIMINATOR: [u8; 8] = [94, 3, 203, 219, 107, 137, 4, 162];
const EXCHANGE_SCALE: u128 = 1_000_000_000_000;
const MAX_RESERVE_AGE_SECONDS: u64 = 180;
pub(super) const METHOD: &str = "jupiter-liquidity-v2-stored-exchange-curve-apr-ppb-v1";

pub(super) fn borrow_apr(
    reserve: &Account,
    rate_model: &Account,
    mint: &str,
    snapshot: &RpcSnapshot,
) -> Result<NativeRate> {
    let data = authenticate(reserve, PROGRAM, 192, &RESERVE_DISCRIMINATOR)?;
    let model = authenticate(rate_model, PROGRAM, 53, &MODEL_DISCRIMINATOR)?;
    let mint = key(mint)?;
    ensure!(
        data[8..40] == mint && model[8..40] == mint && mint.iter().any(|v| *v != 0),
        "Jupiter reserve or model mint does not match configuration"
    );
    ensure!(
        data[40..72].iter().any(|v| *v != 0),
        "Jupiter reserve has no token vault"
    );
    ensure!(
        u16_at(data, 74) <= 10_000 && u16_at(data, 76) <= 10_000 && u16_at(data, 102) <= 10_000,
        "invalid Jupiter reserve utilization or fee"
    );
    let updated = u64_at(data, 78);
    let now = snapshot.observed_at_ms / 1_000;
    ensure!(
        updated > 0 && updated <= now && now - updated <= MAX_RESERVE_AGE_SECONDS,
        "Jupiter stored reserve time is stale or in the future"
    );
    let supply_price = u64_at(data, 86);
    let borrow_price = u64_at(data, 94);
    ensure!(
        supply_price > 0 && borrow_price > 0,
        "Jupiter exchange price is zero"
    );
    // Preserve protocol raw-unit integer division before adding interest-free balances.
    let supply = (u128::from(u64_at(data, 104)) * u128::from(supply_price) / EXCHANGE_SCALE)
        .checked_add(u128::from(u64_at(data, 112)))
        .ok_or_else(|| anyhow!("Jupiter supply overflow"))?;
    let borrowed = (u128::from(u64_at(data, 120)) * u128::from(borrow_price) / EXCHANGE_SCALE)
        .checked_add(u128::from(u64_at(data, 128)))
        .ok_or_else(|| anyhow!("Jupiter borrow overflow"))?;
    ensure!(model[40] == 2, "unsupported Jupiter rate-model version");
    let points = [
        (0, u32::from(u16_at(model, 41))),
        (u32::from(u16_at(model, 43)), u32::from(u16_at(model, 45))),
        (u32::from(u16_at(model, 49)), u32::from(u16_at(model, 51))),
        (10_000, u32::from(u16_at(model, 47))),
    ];
    ensure!(
        points[1].0 > 0 && points[1].0 < points[2].0 && points[2].0 < 10_000,
        "invalid Jupiter model kinks"
    );
    // Interest fee is charged on lender interest. It is not an extra borrower APR.
    Ok(NativeRate {
        apr_ppb: curve_apr(borrowed, supply, &points, 0)?,
        observed_at_ms: (updated * 1_000).min(snapshot.observed_at_ms),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::onchain::tests::{fixture, snapshot, USDG};

    fn reserve() -> Account {
        fixture(
            include_str!("../../../fixtures/native/jupiter-reserve-usdg.b64"),
            PROGRAM,
        )
    }
    fn model() -> Account {
        fixture(
            include_str!("../../../fixtures/native/jupiter-rate-model-usdg.b64"),
            PROGRAM,
        )
    }
    fn rate(reserve: &Account, model: &Account) -> Result<u64> {
        borrow_apr(reserve, model, USDG, &snapshot()).map(|rate| rate.apr_ppb)
    }

    #[test]
    fn public_native_pool_reproduces_the_curve_rate_without_an_api_or_apy() {
        let actual = rate(&reserve(), &model()).unwrap();
        assert_eq!(actual, 54_781_022);
        assert!((actual / 100_000).abs_diff(u64::from(u16_at(&reserve().data, 72))) < 1);
        let mut lender_fee_changed = reserve();
        lender_fee_changed.data[74..76].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(rate(&lender_fee_changed, &model()).unwrap(), actual);
    }

    #[test]
    fn two_kinks_and_interest_free_balances_are_part_of_the_native_calculation() {
        let mut reserve = reserve();
        for (at, value) in [
            (86, 1_000_000_000_000u64),
            (94, 1_000_000_000_000),
            (104, 10_000),
            (112, 0),
            (120, 4_250),
            (128, 0),
        ] {
            reserve.data[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(rate(&reserve, &model()).unwrap(), 25_000_000);
        reserve.data[112..120].copy_from_slice(&10_000u64.to_le_bytes());
        reserve.data[128..136].copy_from_slice(&4_250u64.to_le_bytes());
        assert_eq!(rate(&reserve, &model()).unwrap(), 25_000_000);
        for (borrowed, expected) in [
            (17_000u64, 50_000_000),
            (18_600, 70_000_000),
            (20_000, 1_000_000_000),
        ] {
            reserve.data[120..128].copy_from_slice(&borrowed.to_le_bytes());
            reserve.data[128..136].fill(0);
            assert_eq!(rate(&reserve, &model()).unwrap(), expected);
        }
    }

    #[test]
    fn rejects_owner_executable_short_layout_discriminator_and_wrong_mint_on_either_account() {
        for is_model in [false, true] {
            for kind in 0..5 {
                let (mut reserve, mut model) = (reserve(), model());
                let account = if is_model { &mut model } else { &mut reserve };
                match kind {
                    0 => account.owner = super::super::kamino::PROGRAM.into(),
                    1 => account.executable = true,
                    2 => {
                        account.data.pop();
                    }
                    3 => account.data[0] ^= 1,
                    _ => account.data[8] ^= 1,
                }
                assert!(rate(&reserve, &model).is_err());
            }
        }
    }

    #[test]
    fn rejects_unsupported_version_invalid_curve_prices_balance_and_future_state() {
        for (at, value) in [(40, 1), (43, 0), (49, 0)] {
            let mut model = model();
            model.data[at] = value;
            if at != 40 {
                model.data[at + 1] = 0;
            }
            assert!(rate(&reserve(), &model).is_err());
        }
        let mut invalid_model = model();
        invalid_model.data[45..47].copy_from_slice(&701u16.to_le_bytes());
        assert!(rate(&reserve(), &invalid_model).is_err());
        for at in [86, 94, 104] {
            let mut reserve = reserve();
            reserve.data[at..at + 8].fill(0);
            assert!(rate(&reserve, &model()).is_err());
        }
        let mut reserve = reserve();
        reserve.data[78..86].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(rate(&reserve, &model()).is_err());
        reserve = self::reserve();
        reserve.data[120..128].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(rate(&reserve, &model()).is_err());
    }

    #[test]
    fn stale_stored_exchange_state_does_not_get_freshness_from_a_new_clock() {
        let account = reserve();
        let updated = u64_at(&account.data, 78);
        let mut context = snapshot();
        context.observed_at_ms = (updated + MAX_RESERVE_AGE_SECONDS) * 1_000;
        assert!(borrow_apr(&account, &model(), USDG, &context).is_ok());
        context.observed_at_ms += 1_000;
        assert!(borrow_apr(&account, &model(), USDG, &context).is_err());
        context.observed_at_ms = (updated - 1) * 1_000;
        assert!(borrow_apr(&account, &model(), USDG, &context).is_err());
    }
}
