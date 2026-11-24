//! Kamino Reserve v1, TrueApr basis. Layout provenance is in fixtures/native/README.md.

use anyhow::{ensure, Result};

use super::{authenticate, curve_apr, key, u128_at, u16_at, u32_at, u64_at, NativeRate};
use crate::rpc::{Account, RpcSnapshot};

pub(super) const PROGRAM: &str = "KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD";
const DISCRIMINATOR: [u8; 8] = [43, 242, 204, 202, 26, 247, 59, 127];
const Q60: u128 = 1 << 60;
pub(super) const METHOD: &str = "kamino-reserve-v1-true-apr-curve-ppb-v1";

pub(super) fn borrow_apr(
    account: &Account,
    mint: &str,
    decimals: u8,
    snapshot: &RpcSnapshot,
) -> Result<NativeRate> {
    let data = authenticate(account, PROGRAM, 8_624, &DISCRIMINATOR)?;
    ensure!(u64_at(data, 8) == 1, "unsupported Kamino reserve version");
    ensure!(
        data[128..160] == key(mint)? && data[128..160].iter().any(|v| *v != 0),
        "Kamino reserve mint does not match configuration"
    );
    ensure!(
        decimals <= 18 && u64_at(data, 272) == u64::from(decimals),
        "Kamino reserve decimals do not match configuration"
    );
    ensure!(data[4_856] == 0, "Kamino reserve is not active");
    ensure!(
        data[4_865] == 1,
        "Kamino legacy or unknown rate basis needs a separate model"
    );
    ensure!(
        data[4_863] == 0 && u64_at(data, 5_776) == 0 && u64_at(data, 5_784) == 0,
        "Kamino term debt or early repayment penalty needs a separate model"
    );
    let reserve_slot = u64_at(data, 16);
    let reserve_time = u64::from(u32_at(data, 28));
    let now = snapshot.observed_at_ms / 1_000;
    ensure!(
        reserve_slot > 0 && reserve_slot <= snapshot.slot && snapshot.slot - reserve_slot <= 300,
        "Kamino reserve slot is stale or in the future"
    );
    ensure!(
        reserve_time > 0 && reserve_time <= now && now - reserve_time <= 120,
        "Kamino reserve time is stale or in the future"
    );

    let borrowed = u128_at(data, 232);
    let supply = u128::from(u64_at(data, 224))
        .checked_mul(Q60)
        .and_then(|v| v.checked_add(borrowed))
        .and_then(|v| v.checked_sub(u128_at(data, 344)))
        .and_then(|v| v.checked_sub(u128_at(data, 360)))
        .and_then(|v| v.checked_sub(u128_at(data, 376)))
        .ok_or_else(|| anyhow::anyhow!("invalid Kamino supply or fee balances"))?;
    let mut points: Vec<(u32, u32)> = Vec::with_capacity(11);
    for index in 0..11 {
        let at = 4_920 + index * 8;
        let point = (u32_at(data, at), u32_at(data, at + 4));
        if let Some(last) = points.last().filter(|last| last.0 == 10_000) {
            ensure!(
                point == (0, 0) || point == *last,
                "invalid Kamino curve padding"
            );
        } else {
            ensure!(point.0 <= 10_000, "invalid Kamino curve utilization");
            points.push(point);
        }
    }
    Ok(NativeRate {
        apr_ppb: curve_apr(borrowed, supply, &points, u16_at(data, 4_858))?,
        observed_at_ms: (reserve_time * 1_000).min(snapshot.observed_at_ms),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::sources::onchain::tests::{fixture, snapshot, USDG};

    pub(crate) fn reserve() -> Account {
        fixture(
            include_str!("../../../fixtures/native/kamino-reserve-usdg-main.b64"),
            PROGRAM,
        )
    }
    fn rate(account: &Account) -> Result<u64> {
        borrow_apr(account, USDG, 6, &snapshot()).map(|rate| rate.apr_ppb)
    }

    #[test]
    fn public_native_reserve_curve_includes_host_fee_and_excludes_apy() {
        let account = reserve();
        assert_eq!(account.data[4_865], 1);
        let actual = rate(&account).unwrap();
        assert_eq!(actual, 54_056_439);
        let mut no_host_fee = account.clone();
        no_host_fee.data[4_858..4_860].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(actual - rate(&no_host_fee).unwrap(), 5_000_000);
    }

    #[test]
    fn rejects_wrong_owner_executable_discriminator_size_version_mint_and_decimals() {
        let original = reserve();
        for account in [
            Account {
                owner: super::super::jupiter::PROGRAM.into(),
                ..original.clone()
            },
            Account {
                executable: true,
                ..original.clone()
            },
            Account {
                lamports: 0,
                ..original.clone()
            },
            Account {
                data: original.data[..8_623].to_vec(),
                ..original.clone()
            },
        ] {
            assert!(rate(&account).is_err());
        }
        for at in [0, 8, 128, 272] {
            let mut account = original.clone();
            account.data[at] ^= 1;
            assert!(rate(&account).is_err(), "offset {at}");
        }
        assert!(borrow_apr(&original, USDG, 9, &snapshot()).is_err());
        let mut expanded = original.clone();
        expanded.data.push(0);
        assert!(rate(&expanded).is_err());
    }

    #[test]
    fn rejects_unsupported_basis_inactive_term_debt_and_penalties() {
        for (at, value) in [
            (4_856, 1),
            (4_865, 0),
            (4_865, 2),
            (4_863, 1),
            (5_776, 1),
            (5_784, 1),
        ] {
            let mut account = reserve();
            account.data[at] = value;
            assert!(rate(&account).is_err(), "offset {at}");
        }
    }

    #[test]
    fn rejects_stale_future_and_invalid_utilization_without_clamping() {
        let account = reserve();
        let mut context = snapshot();
        context.slot += 301;
        assert!(borrow_apr(&account, USDG, 6, &context).is_err());
        context = snapshot();
        context.observed_at_ms += 121_000;
        assert!(borrow_apr(&account, USDG, 6, &context).is_err());
        context = snapshot();
        context.slot -= 1;
        assert!(borrow_apr(&account, USDG, 6, &context).is_err());
        context = snapshot();
        context.observed_at_ms -= 1_000;
        assert!(borrow_apr(&account, USDG, 6, &context).is_err());
        let mut invalid = account.clone();
        invalid.data[344..360].copy_from_slice(&u128::MAX.to_le_bytes());
        assert!(rate(&invalid).is_err());
        invalid = account.clone();
        invalid.data[224..232].fill(0);
        assert!(rate(&invalid).is_err());
    }

    #[test]
    fn curve_requires_complete_monotonic_points_and_allows_only_documented_padding() {
        for (index, util, rate_bps) in [
            (0, 1, 0),
            (1, 0, 551),
            (1, 9_500, 4_000),
            (2, 9_999, 3_634),
            (3, 10_000, 3_635),
        ] {
            let mut account = reserve();
            let at = 4_920 + index * 8;
            account.data[at..at + 4].copy_from_slice(&u32::to_le_bytes(util));
            account.data[at + 4..at + 8].copy_from_slice(&u32::to_le_bytes(rate_bps));
            // A missing endpoint must not be supplied by old duplicate padding.
            if index == 2 {
                account.data[at + 8..4_920 + 11 * 8].fill(0);
            }
            assert!(rate(&account).is_err(), "point {index}");
        }
        let mut zero_padding = reserve();
        zero_padding.data[4_920 + 3 * 8..4_920 + 11 * 8].fill(0);
        assert_eq!(rate(&zero_padding).unwrap(), rate(&reserve()).unwrap());
    }
}
