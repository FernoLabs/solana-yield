use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    pub database: String,
    pub interval_seconds: u64,
    pub max_age_seconds: u64,
    #[serde(default)]
    pub allow_api_sources: bool,
    pub rpc_url_env: Option<String>,
    pub genesis_hash: String,
    pub sources: Vec<SourceConfig>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_retention")]
    pub history_retention_days: u64,
    #[serde(default = "default_history_rows")]
    pub max_history_rows: u64,
}

fn default_concurrency() -> usize {
    4
}
fn default_retention() -> u64 {
    30
}
fn default_history_rows() -> u64 {
    250_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "adapter", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceConfig {
    Defillama {
        id: String,
    },
    CatalogApi {
        id: String,
        url_env: String,
    },
    Kamino {
        id: String,
        reserve: String,
        mint: String,
        asset: String,
        decimals: u8,
    },
    Jupiter {
        id: String,
        reserve: String,
        rate_model: String,
        mint: String,
        asset: String,
        decimals: u8,
    },
}

impl SourceConfig {
    pub fn id(&self) -> &str {
        match self {
            Self::Defillama { id, .. }
            | Self::CatalogApi { id, .. }
            | Self::Kamino { id, .. }
            | Self::Jupiter { id, .. } => id,
        }
    }
    pub fn is_api(&self) -> bool {
        matches!(self, Self::Defillama { .. } | Self::CatalogApi { .. })
    }
    pub fn adapter(&self) -> &'static str {
        match self {
            Self::Defillama { .. } => "defillama",
            Self::CatalogApi { .. } => "catalog_api",
            Self::Kamino { .. } => "kamino",
            Self::Jupiter { .. } => "jupiter",
        }
    }
}

impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_mode(true)
    }

    /// Serving saved observations requires no configured collector or RPC URL.
    pub fn validate_server(&self) -> anyhow::Result<()> {
        self.validate_mode(false)
    }

    fn validate_mode(&self, collect: bool) -> anyhow::Result<()> {
        use anyhow::ensure;
        self.listen.parse::<std::net::SocketAddr>()?;
        ensure!(
            !self.database.trim().is_empty(),
            "database path is required"
        );
        ensure!(
            (10..=86400).contains(&self.interval_seconds),
            "collection interval must be between10 and86400 seconds"
        );
        ensure!(
            (1..=86400).contains(&self.max_age_seconds),
            "invalid freshness limit"
        );
        ensure!(
            (1..=16).contains(&self.concurrency),
            "concurrency must be between1 and16"
        );
        ensure!(
            (1..=365).contains(&self.history_retention_days)
                && (1..=1_000_000).contains(&self.max_history_rows),
            "invalid history retention limits"
        );
        ensure!(
            (!collect || !self.sources.is_empty()) && self.sources.len() <= 4096,
            "configure between 1 and 4096 sources for collection"
        );
        crate::rpc::public_key(&self.genesis_hash)?;
        let mut ids = std::collections::BTreeSet::new();
        for source in &self.sources {
            let id = source.id();
            ensure!(
                !id.is_empty()
                    && id.len() <= 80
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b)),
                "invalid source id"
            );
            ensure!(ids.insert(id), "duplicate source id");
            ensure!(
                !source.is_api() || self.allow_api_sources,
                "API sources require allow_api_sources=true"
            );
            match source {
                SourceConfig::Kamino {
                    reserve,
                    mint,
                    decimals,
                    asset,
                    ..
                } => {
                    crate::rpc::public_key(reserve)?;
                    crate::rpc::public_key(mint)?;
                    ensure!(
                        *decimals <= 18 && !asset.trim().is_empty() && asset.len() <= 512,
                        "invalid token configuration"
                    );
                }
                SourceConfig::Jupiter {
                    reserve,
                    rate_model,
                    mint,
                    decimals,
                    asset,
                    ..
                } => {
                    for key in [reserve, rate_model, mint] {
                        crate::rpc::public_key(key)?;
                    }
                    ensure!(
                        reserve != rate_model
                            && *decimals <= 18
                            && !asset.trim().is_empty()
                            && asset.len() <= 512,
                        "invalid token configuration"
                    );
                }
                SourceConfig::CatalogApi { url_env, .. } => {
                    ensure!(valid_env(url_env), "invalid API environment variable name");
                }
                _ => {}
            }
        }
        if collect && self.sources.iter().any(|source| !source.is_api()) {
            ensure!(
                self.rpc_url_env
                    .as_ref()
                    .is_some_and(|name| valid_env(name)),
                "native sources require rpc_url_env"
            );
        }
        Ok(())
    }
}

fn valid_env(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_uppercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}
