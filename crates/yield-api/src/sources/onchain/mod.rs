//! Native account readers. No API rate, price, or assumed slot duration enters these rates.

mod jupiter;
mod kamino;

use std::cmp::Ordering;

use anyhow::{anyhow, ensure, Result};

use crate::{
    config::SourceConfig,
    model::{Evidence, Observation, Origin, Rate, RateKind},
    rpc::{Account, RpcSnapshot},
    sources::Context,
};

const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
const RATE_SCALE: u64 = 1_000_000_000;
const MAX_APR_PPB: u64 = 1_000 * RATE_SCALE;

struct NativeRate {
    apr_ppb: u64,
    observed_at_ms: u64,
}

pub async fn collect(source: &SourceConfig, context: &Context<'_>) -> Result<Vec<Observation>> {
    let addresses = match source {
        SourceConfig::Kamino { reserve, mint, .. } => vec![reserve.clone(), mint.clone()],
        SourceConfig::Jupiter {
            reserve,
            rate_model,
            mint,
            ..
        } => {
            vec![reserve.clone(), rate_model.clone(), mint.clone()]
        }
        _ => return Err(anyhow!("source is not a native adapter")),
    };
    let rpc = context
        .rpc
        .ok_or_else(|| anyhow!("native adapter requires an RPC reader"))?;
    let snapshot = rpc.snapshot(&addresses).await?;
    Ok(vec![from_snapshot(
        source,
        &snapshot,
        context.collected_at_ms,
    )?])
}

fn from_snapshot(
    source: &SourceConfig,
    snapshot: &RpcSnapshot,
    collected_at_ms: u64,
) -> Result<Observation> {
    let (id, reserve, asset, protocol, method, rate, mut accounts) = match source {
        SourceConfig::Kamino {
            id,
            reserve,
            mint,
            asset,
            decimals,
        } => {
            validate_mint(snapshot.get(mint)?, *decimals)?;
            let rate = kamino::borrow_apr(snapshot.get(reserve)?, mint, *decimals, snapshot)?;
            (
                id,
                reserve,
                asset,
                "kamino",
                kamino::METHOD,
                rate,
                vec![reserve.clone(), mint.clone()],
            )
        }
        SourceConfig::Jupiter {
            id,
            reserve,
            rate_model,
            mint,
            asset,
            decimals,
        } => {
            validate_mint(snapshot.get(mint)?, *decimals)?;
            let rate = jupiter::borrow_apr(
                snapshot.get(reserve)?,
                snapshot.get(rate_model)?,
                mint,
                snapshot,
            )?;
            (
                id,
                reserve,
                asset,
                "jupiter-lend",
                jupiter::METHOD,
                rate,
                vec![reserve.clone(), rate_model.clone(), mint.clone()],
            )
        }
        _ => return Err(anyhow!("source is not a native adapter")),
    };
    accounts.push(crate::rpc::CLOCK.into());
    let observation = Observation {
        schema_version: 1,
        id: id.clone(),
        source_id: id.clone(),
        protocol: protocol.into(),
        protocols: vec![protocol.into()],
        market: reserve.clone(),
        asset: asset.clone(),
        denomination: asset.clone(),
        method: method.into(),
        rates: vec![Rate {
            component: "borrow".into(),
            kind: RateKind::Apr,
            value: rate.apr_ppb as f64 / RATE_SCALE as f64,
        }],
        window_seconds: None,
        observed_at_ms: Some(rate.observed_at_ms),
        collected_at_ms,
        unavailable_reason: None,
        evidence: Evidence {
            origin: Origin::Onchain,
            transport: "rpc".into(),
            genesis_hash: Some(snapshot.genesis_hash.clone()),
            commitment: Some("finalized".into()),
            context_slot: Some(snapshot.slot),
            accounts,
        },
    };
    observation.validate()?;
    Ok(observation)
}

fn authenticate<'a>(
    account: &'a Account,
    owner: &str,
    size: usize,
    discriminator: &[u8; 8],
) -> Result<&'a [u8]> {
    ensure!(
        account.owner == owner && !account.executable && account.lamports > 0,
        "invalid native account owner or state"
    );
    ensure!(
        account.data.len() == size && account.data[..8] == *discriminator,
        "unsupported native account layout"
    );
    Ok(&account.data)
}

fn key(value: &str) -> Result<[u8; 32]> {
    crate::rpc::public_key(value)?;
    bs58::decode(value)
        .into_vec()?
        .try_into()
        .map_err(|_| anyhow!("invalid mint key"))
}

fn validate_mint(account: &Account, decimals: u8) -> Result<()> {
    ensure!(
        matches!(account.owner.as_str(), TOKEN | TOKEN_2022)
            && !account.executable
            && account.lamports > 0,
        "invalid token mint owner or state"
    );
    let data = &account.data;
    ensure!(
        data.len() >= 82 && data[45] == 1 && decimals <= 18 && data[44] == decimals,
        "invalid token mint or configured decimals"
    );
    ensure!(
        u32_at(data, 0) <= 1 && u32_at(data, 46) <= 1,
        "invalid token mint authorities"
    );
    if account.owner == TOKEN {
        ensure!(data.len() == 82, "unsupported SPL mint size");
    } else if data.len() != 82 {
        ensure!(
            data.len() >= 166 && data[82..165].iter().all(|v| *v == 0) && data[165] == 1,
            "invalid Token-2022 mint layout"
        );
        let mut seen = std::collections::BTreeSet::new();
        let mut at = 166;
        while at < data.len() && data[at..].iter().any(|v| *v != 0) {
            ensure!(at + 4 <= data.len(), "truncated Token-2022 extension");
            let kind = u16_at(data, at);
            let length = usize::from(u16_at(data, at + 2));
            at += 4;
            ensure!(
                kind != 0 && seen.insert(kind) && at + length <= data.len(),
                "invalid Token-2022 extension"
            );
            at += length;
        }
    }
    Ok(())
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}
fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
fn u128_at(data: &[u8], at: usize) -> u128 {
    u128::from_le_bytes(data[at..at + 16].try_into().unwrap())
}

/// A checked numerator, not a general integer API. Four limbs are sufficient for
/// Q60 account values × utilization bps × curve bps × APR parts per billion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wide([u64; 4]);

impl Wide {
    fn from(value: u128) -> Self {
        Self([value as u64, (value >> 64) as u64, 0, 0])
    }
    fn cmp(self, other: Self) -> Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
    fn mul(self, factor: u64) -> Result<Self> {
        let mut out = [0; 4];
        let mut carry = 0u128;
        for (i, limb) in self.0.into_iter().enumerate() {
            let product = u128::from(limb) * u128::from(factor) + carry;
            out[i] = product as u64;
            carry = product >> 64;
        }
        ensure!(carry == 0, "native rate numerator overflow");
        Ok(Self(out))
    }
    fn add(self, other: Self) -> Result<Self> {
        let mut out = [0; 4];
        let mut carry = 0u128;
        for (i, limb) in self.0.into_iter().enumerate() {
            let sum = u128::from(limb) + u128::from(other.0[i]) + carry;
            out[i] = sum as u64;
            carry = sum >> 64;
        }
        ensure!(carry == 0, "native rate numerator overflow");
        Ok(Self(out))
    }
    fn sub(self, other: Self) -> Result<Self> {
        ensure!(
            self.cmp(other) != Ordering::Less,
            "invalid native rate interval"
        );
        let mut out = [0; 4];
        let mut borrow = false;
        for (i, limb) in self.0.into_iter().enumerate() {
            let (value, first) = limb.overflowing_sub(other.0[i]);
            let (value, second) = value.overflowing_sub(u64::from(borrow));
            out[i] = value;
            borrow = first || second;
        }
        ensure!(!borrow, "invalid native rate interval");
        Ok(Self(out))
    }
    fn quotient(self, denominator: Self) -> Result<u64> {
        ensure!(denominator != Self::from(0), "zero native rate denominator");
        ensure!(
            self.cmp(denominator.mul(MAX_APR_PPB + 1)?) == Ordering::Less,
            "borrow APR exceeds the reviewed range"
        );
        let (mut low, mut high) = (0, MAX_APR_PPB);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if denominator.mul(middle)?.cmp(self) != Ordering::Greater {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        Ok(low)
    }
}

/// Interpolate the exact native rational, then floor only the final APR to ppb.
fn curve_apr(borrowed: u128, supply: u128, points: &[(u32, u32)], fixed_bps: u16) -> Result<u64> {
    ensure!(
        supply > 0 && borrowed <= supply,
        "invalid native utilization"
    );
    ensure!(
        points.len() >= 2 && points.first().unwrap().0 == 0 && points.last().unwrap().0 == 10_000,
        "incomplete native rate curve"
    );
    ensure!(
        points
            .windows(2)
            .all(|p| p[0].0 < p[1].0 && p[0].1 <= p[1].1),
        "invalid native rate curve"
    );
    let at = Wide::from(borrowed).mul(10_000)?;
    let supply = Wide::from(supply);
    for pair in points.windows(2) {
        let ((u0, r0), (u1, r1)) = (pair[0], pair[1]);
        if at.cmp(supply.mul(u64::from(u1))?) != Ordering::Greater {
            let width = u64::from(u1 - u0);
            let position = at.sub(supply.mul(u64::from(u0))?)?;
            let numerator = supply
                .mul(width)?
                .mul(u64::from(r0))?
                .add(position.mul(u64::from(r1 - r0))?)?
                .mul(RATE_SCALE)?;
            let denominator = supply.mul(width)?.mul(10_000)?;
            let rate = numerator
                .quotient(denominator)?
                .checked_add(u64::from(fixed_bps) * (RATE_SCALE / 10_000))
                .ok_or_else(|| anyhow!("native rate overflow"))?;
            ensure!(rate <= MAX_APR_PPB, "borrow APR exceeds the reviewed range");
            return Ok(rate);
        }
    }
    Err(anyhow!("native utilization is outside the curve"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{prelude::BASE64_STANDARD, Engine};
    use std::collections::BTreeMap;

    pub(super) const USDG: &str = "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH";
    pub(super) fn fixture(text: &str, owner: &str) -> Account {
        Account {
            owner: owner.into(),
            lamports: 1,
            executable: false,
            data: BASE64_STANDARD.decode(text.trim()).unwrap(),
        }
    }
    fn mint() -> Account {
        let mut data = vec![0; 82];
        data[44] = 6;
        data[45] = 1;
        Account {
            owner: TOKEN.into(),
            lamports: 1,
            executable: false,
            data,
        }
    }
    pub(super) fn snapshot() -> RpcSnapshot {
        RpcSnapshot {
            genesis_hash: "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d".into(),
            slot: 452_570_160,
            observed_at_ms: 1_790_933_433_000,
            accounts: BTreeMap::new(),
        }
    }

    #[test]
    fn emits_only_native_borrow_apr_with_actual_snapshot_evidence() {
        let reserve = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp";
        let source = SourceConfig::Kamino {
            id: "kamino-usdg".into(),
            reserve: reserve.into(),
            mint: USDG.into(),
            asset: "USDG".into(),
            decimals: 6,
        };
        let mut snapshot = snapshot();
        snapshot
            .accounts
            .insert(reserve.into(), kamino::tests::reserve());
        snapshot.accounts.insert(USDG.into(), mint());
        let value = from_snapshot(&source, &snapshot, snapshot.observed_at_ms + 1_000).unwrap();
        assert_eq!(value.rates.len(), 1);
        assert_eq!(
            (value.rates[0].component.as_str(), &value.rates[0].kind),
            ("borrow", &RateKind::Apr)
        );
        assert_eq!(value.evidence.context_slot, Some(snapshot.slot));
        assert_eq!(value.observed_at_ms, Some(snapshot.observed_at_ms));
        assert_eq!(
            value.evidence.accounts,
            vec![reserve, USDG, crate::rpc::CLOCK]
        );
        assert_eq!(value.evidence.origin, Origin::Onchain);
        assert_eq!(value.evidence.commitment.as_deref(), Some("finalized"));
        assert_eq!(value.protocols, vec!["kamino"]);
        snapshot.observed_at_ms += 10_000;
        let later = from_snapshot(&source, &snapshot, snapshot.observed_at_ms + 1_000).unwrap();
        assert_eq!(later.observed_at_ms, value.observed_at_ms);
        snapshot.accounts.remove(reserve);
        assert!(from_snapshot(&source, &snapshot, snapshot.observed_at_ms).is_err());
    }

    #[test]
    fn jupiter_requires_both_native_accounts_and_records_each_read_account() {
        let reserve = "G4KAenCBfybnZLtVe4LdNrmEtQoW3x93oTowFJUPSvx6";
        let model = "6iHHKAK9Mqjn57CVmWe4szAPyTH8s8pniXSj6vWaKW5r";
        let source = SourceConfig::Jupiter {
            id: "jupiter-usdg".into(),
            reserve: reserve.into(),
            rate_model: model.into(),
            mint: USDG.into(),
            asset: "USDG".into(),
            decimals: 6,
        };
        let mut snapshot = snapshot();
        snapshot.accounts.insert(
            reserve.into(),
            fixture(
                include_str!("../../../fixtures/native/jupiter-reserve-usdg.b64"),
                jupiter::PROGRAM,
            ),
        );
        snapshot.accounts.insert(
            model.into(),
            fixture(
                include_str!("../../../fixtures/native/jupiter-rate-model-usdg.b64"),
                jupiter::PROGRAM,
            ),
        );
        snapshot.accounts.insert(USDG.into(), mint());
        let value = from_snapshot(&source, &snapshot, snapshot.observed_at_ms + 1_000).unwrap();
        assert_eq!(
            value.evidence.accounts,
            vec![reserve, model, USDG, crate::rpc::CLOCK]
        );
        assert_eq!(value.rates.len(), 1);
        assert_eq!(value.rates[0].value, 0.054781022);
        assert_eq!(value.observed_at_ms, Some(1_790_933_281_000));
        assert_eq!(
            value.evidence.genesis_hash.as_deref(),
            Some(snapshot.genesis_hash.as_str())
        );
        snapshot.observed_at_ms += 10_000;
        let later = from_snapshot(&source, &snapshot, snapshot.observed_at_ms + 1_000).unwrap();
        assert_eq!(later.observed_at_ms, value.observed_at_ms);
        snapshot.accounts.remove(model);
        assert!(from_snapshot(&source, &snapshot, snapshot.observed_at_ms).is_err());
    }

    #[test]
    fn mint_owner_decimals_initialization_and_extension_layout_are_checked() {
        let mut account = mint();
        validate_mint(&account, 6).unwrap();
        assert!(validate_mint(&account, 9).is_err());
        account.owner = kamino::PROGRAM.into();
        assert!(validate_mint(&account, 6).is_err());
        account = mint();
        account.data[45] = 0;
        assert!(validate_mint(&account, 6).is_err());
        account = mint();
        account.owner = TOKEN_2022.into();
        account.data.resize(166, 0);
        account.data[165] = 1;
        validate_mint(&account, 6).unwrap();
        account.data.extend_from_slice(&[12, 0, 32, 0]);
        assert!(validate_mint(&account, 6).is_err());
    }

    #[test]
    fn exact_curve_math_covers_endpoints_fees_and_large_q60_balances() {
        let curve = [(0, 0), (8_500, 500), (9_300, 700), (10_000, 10_000)];
        assert_eq!(curve_apr(0, 10_000, &curve, 50).unwrap(), 5_000_000);
        assert_eq!(curve_apr(4_250, 10_000, &curve, 0).unwrap(), 25_000_000);
        assert_eq!(curve_apr(8_500, 10_000, &curve, 0).unwrap(), 50_000_000);
        assert_eq!(curve_apr(9_300, 10_000, &curve, 0).unwrap(), 70_000_000);
        assert_eq!(curve_apr(10_000, 10_000, &curve, 0).unwrap(), RATE_SCALE);
        let large = u128::MAX / 10_000;
        assert_eq!(
            curve_apr(large * 4_250, large * 10_000, &curve, 0).unwrap(),
            25_000_000
        );
        assert!(curve_apr(1, 0, &curve, 0).is_err());
        assert!(curve_apr(2, 1, &curve, 0).is_err());
        assert!(curve_apr(0, 1, &[(0, 20_000_000), (10_000, 20_000_000)], 0).is_err());
    }

    #[test]
    fn checked_wide_math_matches_small_integer_division_and_rejects_overflow() {
        for supply in [1u128, 11, 100, 23_791] {
            for borrowed in [0, supply / 3, supply / 2, supply] {
                let expected = (borrowed * u128::from(RATE_SCALE) / supply) as u64;
                assert_eq!(
                    curve_apr(borrowed, supply, &[(0, 0), (10_000, 10_000)], 0).unwrap(),
                    expected
                );
            }
        }
        assert!(Wide([u64::MAX; 4]).mul(2).is_err());
        assert!(Wide([u64::MAX; 4]).add(Wide::from(1)).is_err());
        assert!(Wide::from(0).sub(Wide::from(1)).is_err());
        assert!(Wide::from(0).quotient(Wide::from(0)).is_err());
    }
}
