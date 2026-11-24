#[tokio::main]
async fn main() {
    if let Err(error) = start().await {
        eprintln!("yield indexer failed: {error}");
        std::process::exit(1);
    }
}

async fn start() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut once = false;
    let mut serve_only = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => path = args.next(),
            "--once" => once = true,
            "--serve-only" => serve_only = true,
            "--help" | "-h" => {
                println!("yield-indexer --config PATH [--once | --serve-only]\nServe saved observations without collectors with --serve-only. Chain and API collectors require explicit configuration.");
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument; use --help"),
        }
    }
    let path = path.ok_or_else(|| anyhow::anyhow!("--config is required"))?;
    let bytes = std::fs::read(path)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "configuration too large");
    let config = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid configuration JSON"))?;
    yield_indexer::service::run_mode(config, once, serve_only).await
}
