use std::collections::HashSet;

use anyhow::{ensure, Result};
use serde_json::Value;

use super::{evidence, identity, number, source_time, text, unavailable, MAX_RECORDS};
use crate::model::{Observation, Rate, RateKind};

pub const POOLS_URL: &str = "https://yields.llama.fi/pools";

/// DefiLlama's APY fields are percentages, not annual fractions. Each
/// supplied component remains separate; no total is manufactured by a sum.
/// Schema: https://github.com/DefiLlama/yield-server#adaptors
pub fn parse(payload: &Value, source_id: &str, collected_at_ms: u64) -> Result<Vec<Observation>> {
    ensure!(
        payload.get("status").and_then(Value::as_str) == Some("success"),
        "invalid DefiLlama response status"
    );
    let rows = payload
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("invalid DefiLlama pool list"))?;
    ensure!(rows.len() <= MAX_RECORDS, "too many DefiLlama pools");
    let mut seen = HashSet::new();
    let mut observations = Vec::new();
    for row in rows {
        if !text(row.get("chain")).is_some_and(|chain| chain.eq_ignore_ascii_case("solana")) {
            continue;
        }
        let Some(pool) = identity(row, &["pool"]) else {
            continue;
        };
        // Avoid truncating identity when adding the provider namespace.
        ensure!(pool.len() <= 506, "DefiLlama pool identity is too long");
        ensure!(
            seen.insert(pool.clone()),
            "duplicate DefiLlama pool identity"
        );
        let protocol = identity(row, &["project"]);
        let asset = identity(row, &["symbol"]);
        let primary = protocol.clone().unwrap_or_else(|| "unknown".into());
        let mut observation = Observation {
            schema_version: 1,
            // This is the existing catalog's provider-pool identity.
            id: format!("llama:{pool}"),
            source_id: source_id.into(),
            protocol: primary.clone(),
            protocols: vec![primary],
            market: pool,
            asset: asset.clone().unwrap_or_else(|| "unknown".into()),
            denomination: "underlying_asset".into(),
            method: "provider_reported_apy".into(),
            rates: Vec::new(),
            window_seconds: None,
            observed_at_ms: source_time(row, collected_at_ms),
            collected_at_ms,
            unavailable_reason: Some("source timestamp is missing or invalid".into()),
            evidence: evidence(),
        };
        if protocol.is_none() || asset.is_none() {
            unavailable(
                &mut observation,
                "pool identity metadata is missing or invalid",
            );
        } else if observation.observed_at_ms.is_some() {
            let mut invalid = false;
            for (field, component) in [
                ("apy", "total"),
                ("apyBase", "base"),
                ("apyReward", "rewards_in_total"),
            ] {
                match row.get(field) {
                    None | Some(Value::Null) => {}
                    Some(value) => match number(value)
                        .map(|n| n / 100.0)
                        .filter(|n| n.is_finite() && *n >= -1.0)
                    {
                        Some(value) => observation.rates.push(Rate {
                            component: component.into(),
                            kind: RateKind::Apy,
                            value,
                        }),
                        None => invalid = true,
                    },
                }
            }
            // An explicit invalid latest total must not be replaced by a
            // component (or an older percentage-change/history field).
            if row.get("apy").is_some_and(Value::is_null) || invalid || observation.rates.is_empty()
            {
                unavailable(&mut observation, "latest pool rate is missing or invalid");
            } else {
                observation.unavailable_reason = None;
            }
        }
        observation.validate()?;
        observations.push(observation);
    }
    Ok(observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Origin;
    use serde_json::json;

    const NOW: u64 = 1_792_108_800_000;
    fn pool() -> Value {
        json!({"pool":"pool-a","chain":"Solana","project":"kamino-lend","symbol":"USDC",
        "apy":5.25,"apyBase":5.0,"apyReward":0.25,"timestamp":"2026-10-16T00:00:00Z"})
    }
    fn parse_row(row: Value) -> Observation {
        parse(&json!({"status":"success","data":[row]}), "llama", NOW)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn percentages_identity_and_api_origin_are_preserved() {
        let mut row = pool();
        row["evidence"] = json!({"origin":"onchain","context_slot":999});
        let got = parse_row(row);
        assert_eq!(got.id, "llama:pool-a");
        assert_eq!(got.source_id, "llama");
        assert_eq!(got.protocols, vec!["kamino-lend"]);
        assert_eq!(
            got.rates.iter().map(|r| r.value).collect::<Vec<_>>(),
            vec![0.0525, 0.05, 0.0025]
        );
        assert_eq!(got.observed_at_ms, Some(NOW));
        assert_eq!(got.evidence.origin, Origin::Api);
        assert_eq!(got.evidence.context_slot, None);
    }

    #[test]
    fn undated_and_invalid_latest_remain_identified_but_unavailable() {
        for bad in [json!(null), json!("5"), json!(-101)] {
            let mut row = pool();
            row["apy"] = bad;
            let got = parse_row(row);
            assert_eq!(got.id, "llama:pool-a");
            assert!(got.rates.is_empty());
            assert!(got.unavailable_reason.is_some());
        }
        let mut row = pool();
        row.as_object_mut().unwrap().remove("timestamp");
        row["generated_at"] = json!(NOW / 1000);
        let got = parse_row(row);
        assert_eq!(got.observed_at_ms, None);
        assert!(got.rates.is_empty());
    }

    #[test]
    fn zero_is_valid_and_only_solana_is_admitted() {
        let mut zero = pool();
        zero["apy"] = json!(0);
        zero["apyBase"] = json!(0);
        zero["apyReward"] = Value::Null;
        assert_eq!(parse_row(zero).rates[0].value, 0.0);
        let mut other = pool();
        other["chain"] = json!("Ethereum");
        let mut unknown = pool();
        unknown["pool"] = Value::Null;
        assert!(parse(
            &json!({"status":"success","data":[other,unknown]}),
            "llama",
            NOW
        )
        .unwrap()
        .is_empty());
        assert!(parse(&json!({"status":"failed","data":[]}), "llama", NOW).is_err());
    }
}
