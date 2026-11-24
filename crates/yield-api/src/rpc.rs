use crate::sources::bounded_json;
use anyhow::{anyhow, ensure, Result};
use base64::{prelude::BASE64_STANDARD, Engine};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::OnceCell;

pub const CLOCK: &str = "SysvarC1ock11111111111111111111111111111111";
const SYSVAR_OWNER: &str = "Sysvar1111111111111111111111111111111111111";

#[derive(Clone, Debug)]
pub struct Account {
    pub owner: String,
    pub lamports: u64,
    pub executable: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct RpcSnapshot {
    pub genesis_hash: String,
    pub slot: u64,
    pub observed_at_ms: u64,
    pub accounts: BTreeMap<String, Account>,
}

impl RpcSnapshot {
    pub fn get(&self, address: &str) -> Result<&Account> {
        self.accounts
            .get(address)
            .ok_or_else(|| anyhow!("required chain account is missing"))
    }
}

#[derive(Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    url: reqwest::Url,
    expected_genesis: String,
    genesis: Arc<OnceCell<String>>,
}

pub fn public_key(value: &str) -> Result<()> {
    let bytes = bs58::decode(value)
        .into_vec()
        .map_err(|_| anyhow!("invalid public key"))?;
    ensure!(
        bytes.len() == 32 && bs58::encode(&bytes).into_string() == value,
        "invalid public key"
    );
    Ok(())
}

pub fn endpoint(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).map_err(|_| anyhow!("invalid endpoint URL"))?;
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    ensure!(
        url.scheme() == "https" || url.scheme() == "http" && local,
        "endpoint needs HTTPS or loopback HTTP"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
        "endpoint user information and fragments are not supported"
    );
    Ok(url)
}

impl RpcClient {
    pub fn new(http: reqwest::Client, url: &str, expected_genesis: &str) -> Result<Self> {
        public_key(expected_genesis)?;
        Ok(Self {
            http,
            url: endpoint(url)?,
            expected_genesis: expected_genesis.to_string(),
            genesis: Arc::new(OnceCell::new()),
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let response = self
            .http
            .post(self.url.clone())
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| anyhow!("RPC request failed"))?;
        let value = bounded_json(response, 2 * 1024 * 1024)
            .await
            .map_err(|_| anyhow!("RPC response invalid or too large"))?;
        ensure!(
            value.get("error").is_none()
                && value.get("result").is_some()
                && value["id"] == 1
                && value["jsonrpc"] == "2.0",
            "RPC returned an error or invalid envelope"
        );
        Ok(value["result"].clone())
    }

    pub async fn snapshot(&self, addresses: &[String]) -> Result<RpcSnapshot> {
        ensure!(
            !addresses.is_empty() && addresses.len() <= 99,
            "invalid account batch size"
        );
        let genesis = self
            .genesis
            .get_or_try_init(|| async {
                let value = self.call("getGenesisHash", json!([])).await?;
                ensure!(
                    value.as_str() == Some(&self.expected_genesis),
                    "RPC genesis does not match the configured chain"
                );
                Ok::<_, anyhow::Error>(self.expected_genesis.clone())
            })
            .await?
            .clone();
        let mut keys = addresses.to_vec();
        for key in &keys {
            public_key(key)?;
        }
        ensure!(
            !keys.iter().any(|key| key == CLOCK),
            "Clock is supplied by the reader"
        );
        keys.push(CLOCK.into());
        let answer = self
            .call(
                "getMultipleAccounts",
                json!([keys,{"encoding":"base64","commitment":"finalized"}]),
            )
            .await?;
        let slot = answer["context"]["slot"]
            .as_u64()
            .filter(|slot| *slot > 0)
            .ok_or_else(|| anyhow!("missing account context slot"))?;
        let values = answer["value"]
            .as_array()
            .ok_or_else(|| anyhow!("missing account values"))?;
        ensure!(values.len() == keys.len(), "incomplete account batch");
        let mut accounts = BTreeMap::new();
        for (key, value) in keys.iter().zip(values) {
            if value.is_null() {
                continue;
            }
            let owner = value["owner"]
                .as_str()
                .ok_or_else(|| anyhow!("missing account owner"))?;
            public_key(owner)?;
            let data = value["data"]
                .as_array()
                .filter(|data| data.len() == 2 && data[1] == "base64")
                .ok_or_else(|| anyhow!("unexpected account encoding"))?;
            let wire = data[0]
                .as_str()
                .filter(|s| s.len() <= 128 * 1024)
                .ok_or_else(|| anyhow!("account data too large"))?;
            let data = BASE64_STANDARD
                .decode(wire)
                .map_err(|_| anyhow!("invalid account data"))?;
            ensure!(data.len() <= 96 * 1024, "account data too large");
            accounts.insert(
                key.clone(),
                Account {
                    owner: owner.into(),
                    lamports: value["lamports"]
                        .as_u64()
                        .ok_or_else(|| anyhow!("missing lamports"))?,
                    executable: value["executable"]
                        .as_bool()
                        .ok_or_else(|| anyhow!("missing executable state"))?,
                    data,
                },
            );
        }
        let clock = accounts
            .remove(CLOCK)
            .ok_or_else(|| anyhow!("chain Clock is missing"))?;
        ensure!(
            clock.owner == SYSVAR_OWNER && !clock.executable && clock.data.len() == 40,
            "invalid chain Clock"
        );
        ensure!(
            u64::from_le_bytes(clock.data[..8].try_into()?) == slot,
            "chain Clock and response context do not match"
        );
        let timestamp = i64::from_le_bytes(clock.data[32..40].try_into()?);
        let observed_at_ms = u64::try_from(timestamp)?
            .checked_mul(1000)
            .ok_or_else(|| anyhow!("invalid chain time"))?;
        let now = crate::service::now_ms();
        ensure!(
            observed_at_ms > 0
                && observed_at_ms <= now.saturating_add(30_000)
                && now.saturating_sub(observed_at_ms) <= 180_000,
            "chain Clock is stale or in the future"
        );
        Ok(RpcSnapshot {
            genesis_hash: genesis,
            slot,
            observed_at_ms,
            accounts,
        })
    }
}
