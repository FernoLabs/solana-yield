use std::collections::HashSet;

use anyhow::{ensure, Result};
use serde_json::Value;

use super::{evidence, identity, number, source_time, text, unavailable, MAX_RECORDS};
use crate::model::{Observation, Rate, RateKind};

/// Import the existing Solana ranking without inventing a new identity or
/// treating its API-reported provenance as direct account evidence.
pub fn parse(payload: &Value, source_id: &str, collected_at_ms: u64) -> Result<Vec<Observation>> {
    let rows = payload
        .get("rows")
        .unwrap_or(payload)
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid catalog row list"))?;
    ensure!(rows.len() <= MAX_RECORDS, "too many catalog rows");
    // The page's first paint contains only visible rows, while its coverage
    // can describe the full catalog. Only the complete ranking can replace
    // this source's current set of positions.
    ensure!(
        payload.get("first_paint").is_none(),
        "catalog response is partial"
    );
    if let Some(coverage) = payload.get("coverage") {
        ensure!(
            coverage.get("positions").and_then(Value::as_u64) == Some(rows.len() as u64),
            "catalog coverage does not match its rows"
        );
    }
    let mut observations = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        // The existing ranking is Solana-only and has no chain field. An
        // explicit different/invalid chain must not enter that catalog.
        if row.get("chain").is_some_and(|chain| {
            !text(Some(chain)).is_some_and(|s| s.eq_ignore_ascii_case("solana"))
        }) {
            continue;
        }
        let id = identity(row, &["id"])
            .ok_or_else(|| anyhow::anyhow!("catalog row identity is missing or invalid"))?;
        ensure!(seen.insert(id.clone()), "duplicate catalog row identity");
        let protocol = identity(row, &["protocol", "venue"]).or_else(|| {
            row.get("protocols")
                .and_then(Value::as_array)
                .and_then(|p| p.first())
                .and_then(|p| text(Some(p)))
                .map(str::to_owned)
        });
        let market = identity(row, &["market", "name"]);
        let asset = identity(row, &["asset", "token"]);
        let primary = protocol.clone().unwrap_or_else(|| "unknown".into());
        let mut protocols = vec![primary.clone()];
        let mut invalid_protocols = false;
        if let Some(value) = row.get("protocols") {
            if let Some(values) = value.as_array() {
                for value in values {
                    if let Some(name) = text(Some(value)) {
                        if !protocols.iter().any(|existing| existing == name) {
                            if protocols.len() < 64 {
                                protocols.push(name.into());
                            } else {
                                invalid_protocols = true;
                            }
                        }
                    } else {
                        invalid_protocols = true;
                    }
                }
            } else {
                invalid_protocols = true;
            }
        }
        let mut observation = Observation {
            schema_version: 1,
            id,
            source_id: source_id.into(),
            protocol: primary,
            protocols,
            market: market.clone().unwrap_or_else(|| "unknown".into()),
            asset: asset.clone().unwrap_or_else(|| "unknown".into()),
            denomination: "provider_quote".into(),
            method: "catalog_reported_apy".into(),
            rates: Vec::new(),
            window_seconds: None,
            observed_at_ms: source_time(row, collected_at_ms),
            collected_at_ms,
            unavailable_reason: Some("source timestamp is missing or invalid".into()),
            evidence: evidence(),
        };
        if protocol.is_none() || market.is_none() || asset.is_none() || invalid_protocols {
            unavailable(
                &mut observation,
                "catalog identity metadata is missing or invalid",
            );
        } else if row.get("unavailable_reason").is_some_and(|v| !v.is_null()) {
            unavailable(
                &mut observation,
                "provider marked the latest rate unavailable",
            );
        } else if observation.observed_at_ms.is_some() {
            // Missing/null/invalid latest is not an invitation to reuse the
            // ranked value or the weekly history.
            let latest = row
                .get("apy_latest")
                .and_then(number)
                .filter(|n| *n >= -1.0);
            if let Some(value) = latest {
                observation.rates.push(Rate {
                    component: "latest_total".into(),
                    kind: RateKind::Apy,
                    value,
                });
                let mut invalid = false;
                for (field, component) in [
                    ("rewards_apy", "rewards_in_total"),
                    ("incentives_apy", "extra_incentives"),
                ] {
                    match row.get(field) {
                        None | Some(Value::Null) => {}
                        Some(value) => match number(value).filter(|n| *n >= -1.0) {
                            Some(value) => observation.rates.push(Rate {
                                component: component.into(),
                                kind: RateKind::Apy,
                                value,
                            }),
                            None => invalid = true,
                        },
                    }
                }
                if invalid {
                    unavailable(&mut observation, "latest catalog rate component is invalid");
                } else {
                    observation.unavailable_reason = None;
                }
            } else {
                unavailable(
                    &mut observation,
                    "latest catalog rate is missing or invalid",
                );
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
    fn row() -> Value {
        json!({"id":"exponent:original-vault","venue":"Exponent","name":"PT-ONyc",
        "token":"PT-ONyc","source":"exponent","protocols":["Exponent","OnRe"],
        "apy_latest":0.0525,"apy":0.05,"apy_7d":0.05,"incentives_apy":0.002,"rewards_apy":0.001,
        "observed_at":NOW / 1000 - 400})
    }
    fn parse_row(row: Value) -> Observation {
        parse(
            &json!({"rows":[row],"generated_at":NOW / 1000}),
            "original-catalog",
            NOW,
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn original_identity_fraction_time_and_component_meanings_are_preserved() {
        let mut row = row();
        row["evidence"] = json!({"origin":"onchain","commitment":"finalized","context_slot":999});
        let got = parse_row(row);
        assert_eq!(got.id, "exponent:original-vault");
        assert_eq!(got.source_id, "original-catalog");
        assert_eq!(got.protocol, "Exponent");
        assert_eq!(got.protocols, vec!["Exponent", "OnRe"]);
        assert_eq!(
            got.rates
                .iter()
                .map(|r| (r.component.as_str(), r.value))
                .collect::<Vec<_>>(),
            vec![
                ("latest_total", 0.0525),
                ("rewards_in_total", 0.001),
                ("extra_incentives", 0.002)
            ]
        );
        assert_eq!(got.observed_at_ms, Some(NOW - 400_000));
        assert_eq!(got.evidence.origin, Origin::Api);
        assert_eq!(got.evidence.context_slot, None);
    }

    #[test]
    fn invalid_latest_cannot_fall_back_to_rank_or_history() {
        for bad in [Value::Null, json!("0.05"), json!(-1.01), json!({})] {
            let mut row = row();
            row["apy_latest"] = bad;
            let got = parse_row(row);
            assert_eq!(got.id, "exponent:original-vault");
            assert!(got.rates.is_empty());
            assert_eq!(
                got.unavailable_reason.as_deref(),
                Some("latest catalog rate is missing or invalid")
            );
        }
        let mut missing = row();
        missing.as_object_mut().unwrap().remove("apy_latest");
        assert!(parse_row(missing).rates.is_empty());
    }

    #[test]
    fn missing_date_cannot_be_replaced_by_generated_or_fetched_time() {
        let mut row = row();
        row.as_object_mut().unwrap().remove("observed_at");
        row["collected_at_ms"] = json!(NOW);
        row["generated_at"] = json!(NOW / 1000);
        let got = parse_row(row);
        assert_eq!(got.observed_at_ms, None);
        assert!(got.rates.is_empty());
        let mut row = self::row();
        row["observed_at"] = json!(NOW / 1000 + 31);
        assert_eq!(parse_row(row).observed_at_ms, None);
    }

    #[test]
    fn zero_latest_is_valid_even_when_rank_is_null() {
        let mut row = row();
        row["apy_latest"] = json!(0);
        row["apy"] = Value::Null;
        let got = parse_row(row);
        assert_eq!(got.rates[0].value, 0.0);
        assert!(got.unavailable_reason.is_none());
    }

    #[test]
    fn known_bad_metadata_is_unavailable_and_missing_identity_rejects_the_complete_set() {
        let mut known = row();
        known["token"] = Value::Null;
        assert!(parse_row(known).rates.is_empty());
        let mut bad_lineage = row();
        bad_lineage["protocols"] = json!(["OnRe", "OnRe", null]);
        let unavailable = parse_row(bad_lineage);
        assert!(unavailable.rates.is_empty());
        assert_eq!(unavailable.protocols, vec!["Exponent", "OnRe"]);
        let mut excessive_lineage = row();
        excessive_lineage["protocols"] =
            json!((0..65).map(|n| format!("P{n}")).collect::<Vec<_>>());
        let unavailable = parse_row(excessive_lineage);
        assert_eq!(unavailable.protocols.len(), 64);
        assert!(unavailable.rates.is_empty());
        let mut unknown = row();
        unknown["id"] = Value::Null;
        let mut other = row();
        other["chain"] = json!("Ethereum");
        assert!(parse(&json!([unknown]), "catalog", NOW).is_err());
        assert!(parse(&json!([other]), "catalog", NOW).unwrap().is_empty());
        let duplicate = row();
        assert!(parse(&json!([duplicate.clone(), duplicate]), "catalog", NOW).is_err());
        assert!(parse(&json!({"rows":null}), "catalog", NOW).is_err());
    }

    #[test]
    fn partial_or_inconsistent_catalog_cannot_replace_the_complete_current_set() {
        for payload in [
            json!({"rows":[],"coverage":{"positions":400}}),
            json!({"rows":[row()],"coverage":{"positions":"1"}}),
            json!({"rows":[row()],"coverage":{"positions":1},"first_paint":{}}),
        ] {
            assert!(parse(&payload, "catalog", NOW).is_err());
        }
        assert!(parse(
            &json!({"rows":[],"coverage":{"positions":0}}),
            "catalog",
            NOW
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            parse(
                &json!({"rows":[row()],"coverage":{"positions":1}}),
                "catalog",
                NOW
            )
            .unwrap()
            .len(),
            1
        );
    }
}
