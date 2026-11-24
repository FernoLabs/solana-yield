use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Onchain,
    Api,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateKind {
    Apr,
    Apy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    pub component: String,
    pub kind: RateKind,
    /// Annual fraction: 0.05 means 5 percent. Components are never summed implicitly.
    pub value: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub origin: Origin,
    pub transport: String,
    pub genesis_hash: Option<String>,
    pub commitment: Option<String>,
    pub context_slot: Option<u64>,
    pub accounts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema_version: u32,
    pub id: String,
    pub source_id: String,
    pub protocol: String,
    pub protocols: Vec<String>,
    pub market: String,
    pub asset: String,
    pub denomination: String,
    pub method: String,
    pub rates: Vec<Rate>,
    pub window_seconds: Option<u64>,
    /// Time of the chain Clock or of the original provider observation.
    pub observed_at_ms: Option<u64>,
    pub collected_at_ms: u64,
    pub unavailable_reason: Option<String>,
    pub evidence: Evidence,
}

impl Observation {
    pub fn normalize_protocols(&mut self) {
        self.protocol = canonical_protocol(&self.protocol).into();
        self.protocols = self
            .protocols
            .iter()
            .map(|name| canonical_protocol(name).to_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "unsupported observation version");
        for value in [
            &self.id,
            &self.source_id,
            &self.protocol,
            &self.market,
            &self.asset,
            &self.denomination,
            &self.method,
        ] {
            ensure!(
                !value.trim().is_empty() && value.len() <= 512,
                "invalid observation identity"
            );
        }
        ensure!(self.rates.len() <= 16, "too many rate components");
        ensure!(
            self.collected_at_ms > 0 && self.collected_at_ms <= i64::MAX as u64,
            "invalid collection time"
        );
        let components: std::collections::BTreeSet<_> =
            self.rates.iter().map(|rate| &rate.component).collect();
        ensure!(
            components.len() == self.rates.len(),
            "duplicate rate component"
        );
        let protocols: std::collections::BTreeSet<_> = self.protocols.iter().collect();
        ensure!(
            !protocols.is_empty()
                && protocols.len() <= 64
                && protocols.len() == self.protocols.len()
                && self.protocols.contains(&self.protocol)
                && self
                    .protocols
                    .iter()
                    .all(|p| !p.trim().is_empty() && p.len() <= 512),
            "invalid protocol lineage"
        );
        ensure!(
            self.unavailable_reason
                .as_ref()
                .is_none_or(|r| !r.trim().is_empty() && r.len() <= 1024),
            "invalid unavailable reason"
        );
        ensure!(
            (self.rates.is_empty()) == self.unavailable_reason.is_some(),
            "missing rates require an explicit unavailable reason"
        );
        for rate in &self.rates {
            ensure!(
                !rate.component.is_empty() && rate.component.len() <= 64 && rate.value.is_finite(),
                "invalid rate"
            );
            ensure!(
                rate.kind != RateKind::Apy || rate.value >= -1.0,
                "invalid APY"
            );
        }
        ensure!(
            self.observed_at_ms
                .is_none_or(|at| at > 0 && at <= self.collected_at_ms.saturating_add(30_000)),
            "invalid observation time"
        );
        ensure!(
            self.rates.is_empty() || self.observed_at_ms.is_some(),
            "a rate needs its source time"
        );
        if self.evidence.origin == Origin::Onchain {
            ensure!(
                self.evidence
                    .genesis_hash
                    .as_ref()
                    .is_some_and(|s| !s.is_empty())
                    && self.evidence.context_slot.is_some_and(|s| s > 0)
                    && self.evidence.commitment.as_deref() == Some("finalized")
                    && !self.evidence.accounts.is_empty(),
                "incomplete chain evidence"
            );
            crate::rpc::public_key(self.evidence.genesis_hash.as_deref().unwrap())?;
            ensure!(
                self.evidence.accounts.len() <= 100,
                "too many evidence accounts"
            );
            for account in &self.evidence.accounts {
                crate::rpc::public_key(account)?;
            }
        } else {
            ensure!(
                self.evidence.genesis_hash.is_none()
                    && self.evidence.commitment.is_none()
                    && self.evidence.context_slot.is_none()
                    && self.evidence.accounts.is_empty(),
                "API observations cannot claim native account evidence"
            );
        }
        Ok(())
    }
}

/// Only reviewed aliases are merged. An unknown name retains its identity.
pub fn canonical_protocol(value: &str) -> &str {
    let key: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match key.as_str() {
        "kamino" | "kaminolend" | "kaminoliquidity" => "Kamino",
        "jupiter" | "jupiterlend" | "juplend" => "Jupiter",
        _ => value,
    }
}
