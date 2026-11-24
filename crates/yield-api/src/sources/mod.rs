pub mod api;
pub mod onchain;

use crate::{config::SourceConfig, model::Observation, rpc::RpcClient};
use anyhow::Result;

pub struct Context<'a> {
    pub http: &'a reqwest::Client,
    pub rpc: Option<&'a RpcClient>,
    pub collected_at_ms: u64,
}

pub async fn collect(source: &SourceConfig, context: &Context<'_>) -> Result<Vec<Observation>> {
    if source.is_api() {
        api::collect(source, context).await
    } else {
        onchain::collect(source, context).await
    }
}

pub async fn bounded_json(
    response: reqwest::Response,
    maximum: usize,
) -> Result<serde_json::Value> {
    use futures::StreamExt;
    anyhow::ensure!(
        response.status().is_success(),
        "source returned a non-success status"
    );
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|n| n <= maximum as u64),
        "source response too large"
    );
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("source response read failed"))?;
        anyhow::ensure!(
            bytes.len().saturating_add(chunk.len()) <= maximum,
            "source response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
