use anyhow::{ensure, Context, Result};
use blockzilla_bank_client::{BankClient, ClientConfig, SourceIdentity, StreamEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use yield_stream::{
    archive,
    pipeline::{parse_block, PARSER_VERSION},
    store::StreamStore,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    url: String,
    key_file: PathBuf,
    source_file: PathBuf,
    database: PathBuf,
    /// Save at most one admitted block with a transaction candidate for offline replay.
    sample_frame: Option<PathBuf>,
}

fn small_file(path: &Path, private: bool) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path).context("read input metadata")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 65_536,
        "input must be a small regular file"
    );
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "key file must not allow group or other access"
        );
    }
    let _ = private;
    Ok(std::fs::read(path)?)
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn capture(config: Config, duration: u64) -> Result<Value> {
    let source: SourceIdentity = serde_json::from_slice(&small_file(&config.source_file, false)?)
        .map_err(|_| anyhow::anyhow!("invalid pinned source JSON"))?;
    source.validate()?;
    let mut sample_saved = if let Some(path) = &config.sample_frame {
        if path.exists() {
            let metadata = std::fs::symlink_metadata(path)?;
            ensure!(
                metadata.file_type().is_file()
                    && metadata.len() <= blockzilla_bank_stream_v2::MAX_FRAME as u64,
                "invalid existing sample frame"
            );
            let frame = blockzilla_bank_client::decode(std::fs::read(path)?.into())?;
            ensure!(
                frame.source == source && !parse_block(&frame)?.candidates.is_empty(),
                "existing sample is not a matching source block with candidates"
            );
            true
        } else {
            false
        }
    } else {
        false
    };
    let key = String::from_utf8(small_file(&config.key_file, true)?)
        .map_err(|_| anyhow::anyhow!("invalid key encoding"))?;
    let client = BankClient::new(ClientConfig::new(&config.url, key.trim(), source.clone()))?;
    let mut store = StreamStore::open(&config.database, &source, &config.url)?;
    let saved = store.cursor()?;
    let started = Instant::now();
    // A finite test deadline also covers bootstrap and connection setup.
    let stop = async move {
        if duration == 0 {
            shutdown().await;
        } else {
            tokio::select! { _ = shutdown() => {}, _ = tokio::time::sleep(Duration::from_secs(duration)) => {} }
        }
    };
    tokio::pin!(stop);
    let setup = async {
        let mut bootstrap = client.bootstrap(saved.as_deref()).await?;
        if saved.is_none() {
            let payload = bootstrap
                .payload
                .as_ref()
                .context("stream has no confirmed capture payload")?;
            let control = bootstrap
                .control
                .as_ref()
                .context("stream has no confirmed capture control")?;
            control.envelope.event.matches_payload(payload)?;
            if let Some(earliest) = &bootstrap.head.earliest_after {
                ensure!(
                    blockzilla_bank_client::parse_cursor(&payload.cursor)?.1
                        >= blockzilla_bank_client::parse_cursor(earliest)?.1,
                    "capture payload is outside replay retention"
                );
            }
            // Replay all controls and children after this exact processed bank.
            // The newer head is source proof, not permission to skip that suffix.
            bootstrap.after = payload.cursor.clone();
            let frame = payload.clone();
            let parsed = tokio::task::spawn_blocking(move || parse_block(&frame)).await??;
            store.initialize(&bootstrap.after, payload, &parsed)?;
        }
        client.connect(bootstrap).await.map_err(anyhow::Error::from)
    };
    let mut stream = tokio::select! {
        _ = &mut stop => anyhow::bail!("capture stopped before the stream connection was established"),
        result = setup => result?,
    };
    let mut deliveries = 0_u64;
    let mut status_messages = 0_u64;
    let mut last_status = None;
    let mut last_report = Instant::now();
    let reason;
    loop {
        let event = tokio::select! {
            _ = &mut stop => { reason = "deadline_or_signal"; break; },
            event = stream.next() => event?,
        };
        match event {
            StreamEvent::Status(head) => {
                status_messages += 1;
                last_status = Some(head.status);
            }
            StreamEvent::Bank(delivery) => {
                let frame = delivery.frame.clone();
                let dependency = delivery.dependency.clone();
                let (parsed, dependency_parsed) = tokio::task::spawn_blocking(move || {
                    let parsed = (frame.envelope.event.kind == "block")
                        .then(|| parse_block(&frame))
                        .transpose()?;
                    let dependency_parsed = dependency.as_ref().map(parse_block).transpose()?;
                    Ok::<_, anyhow::Error>((parsed, dependency_parsed))
                })
                .await??;
                if !sample_saved
                    && parsed
                        .as_ref()
                        .is_some_and(|block| !block.candidates.is_empty())
                {
                    if let Some(path) = &config.sample_frame {
                        use std::io::Write;
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(path)
                            .context("create sample frame; choose a new path for each capture")?;
                        file.write_all(&delivery.frame.wire)?;
                        file.sync_all()?;
                        sample_saved = true;
                    }
                }
                // Commit all parser effects and the exact cursor before ACK.
                store.apply(
                    &delivery.frame,
                    delivery.dependency.as_ref().zip(dependency_parsed.as_ref()),
                    parsed.as_ref(),
                )?;
                stream
                    .ack(&delivery.frame.cursor, || async {
                        ensure!(
                            store.cursor()?.as_deref() == Some(delivery.frame.cursor.as_str()),
                            "durable cursor does not match acknowledgement"
                        );
                        Ok::<(), anyhow::Error>(())
                    })
                    .await?;
                deliveries += 1;
                if last_report.elapsed() >= Duration::from_secs(10) {
                    eprintln!(
                        "{}",
                        json!({"elapsed_seconds":started.elapsed().as_secs(),"deliveries":deliveries,"database":store.summary()?})
                    );
                    last_report = Instant::now();
                }
            }
        }
    }
    Ok(
        json!({"schema_version":1,"input":"blockzilla-bank-v2","parser":PARSER_VERSION,
        "stop_reason":reason,"resumed":saved.is_some(),"elapsed_seconds":started.elapsed().as_secs_f64(),
        "deliveries":deliveries,"status_messages":status_messages,"last_status":last_status,
        "sample_saved":sample_saved,
        "rpc_configured":false,"yield_api_configured":false,"database":store.summary()?}),
    )
}

async fn start() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut config, mut report, mut duration, mut replay, mut fetch) =
        (None, None, 60_u64, None, None);
    let mut archive_manifest = None;
    let mut check_continuity = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = Some(PathBuf::from(args.next().context("missing config path")?)),
            "--report" => report = Some(PathBuf::from(args.next().context("missing report path")?)),
            "--replay-frame" => {
                replay = Some(PathBuf::from(args.next().context("missing replay path")?))
            }
            "--replay-archive" => {
                archive_manifest = Some(PathBuf::from(
                    args.next().context("missing archive manifest path")?,
                ))
            }
            "--fetch-frame" => fetch = Some(args.next().context("missing frame digest")?),
            "--check-continuity" => check_continuity = true,
            "--duration-seconds" => duration = args.next().context("missing duration")?.parse()?,
            "--help" | "-h" => {
                println!(concat!(
                    "yield-stream --config PATH [--duration-seconds 60] [--report PATH] ",
                    "[--replay-frame PATH | --fetch-frame SHA256 | --check-continuity]\n",
                    "yield-stream --replay-archive MANIFEST [--report PATH]\n",
                    "Live capture stores Jupiter instruction candidates. Zero duration runs until a signal or capacity limit.\n",
                    "Replay and continuity checks work offline. Fetch checks one retained block without advancing the capture cursor.\n",
                    "Archive replay reads a bounded local range without a stream configuration or a key."
                ));
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument; use --help"),
        }
    }
    ensure!(duration <= 86_400, "duration must be at most one day");
    ensure!(
        usize::from(replay.is_some())
            + usize::from(fetch.is_some())
            + usize::from(check_continuity)
            + usize::from(archive_manifest.is_some())
            <= 1,
        "select only one offline check or retained fetch"
    );
    if let Some(manifest) = archive_manifest {
        ensure!(
            config.is_none(),
            "archive replay takes a manifest instead of a stream configuration"
        );
        let result = archive::replay(&manifest)?;
        if let Some(path) = report {
            std::fs::write(path, serde_json::to_vec_pretty(&result)?)?;
        }
        println!("{result}");
        return Ok(());
    }
    let config: Config = serde_json::from_slice(&small_file(
        &config.context("--config is required")?,
        false,
    )?)
    .map_err(|_| anyhow::anyhow!("invalid stream configuration"))?;
    let result = if check_continuity {
        ensure!(
            config.database.is_file(),
            "saved stream database is required"
        );
        let source: SourceIdentity =
            serde_json::from_slice(&small_file(&config.source_file, false)?)?;
        let store = StreamStore::open(&config.database, &source, &config.url)?;
        json!({"schema_version":1,"input":"saved_bank_evidence","network_used":false,"database":store.summary()?})
    } else if let Some(path) = replay {
        let source: SourceIdentity =
            serde_json::from_slice(&small_file(&config.source_file, false)?)?;
        source.validate()?;
        let metadata = std::fs::metadata(&path)?;
        ensure!(
            metadata.is_file() && metadata.len() <= blockzilla_bank_stream_v2::MAX_FRAME as u64,
            "invalid replay frame size"
        );
        let frame = blockzilla_bank_client::decode(std::fs::read(path)?.into())?;
        ensure!(
            frame.source == source,
            "replay source does not match pinned source"
        );
        json!({"schema_version":1,"input":"offline_blockzilla_bank_frame","frame_sha256":frame.sha256,
            "bank":frame.envelope.event.bank,"commitment":"processed","block":parse_block(&frame)?})
    } else if let Some(digest) = fetch {
        ensure!(
            blockzilla_bank_stream_v2::is_digest(&digest),
            "invalid frame digest"
        );
        let source: SourceIdentity =
            serde_json::from_slice(&small_file(&config.source_file, false)?)?;
        source.validate()?;
        let key = String::from_utf8(small_file(&config.key_file, true)?)
            .map_err(|_| anyhow::anyhow!("invalid key encoding"))?;
        let client = BankClient::new(ClientConfig::new(&config.url, key.trim(), source))?;
        let frame = tokio::time::timeout(Duration::from_secs(30), client.fetch(&digest))
            .await
            .context("retained frame fetch timed out")??;
        json!({"schema_version":1,"input":"retained_blockzilla_bank_frame","frame_sha256":frame.sha256,
            "bank":frame.envelope.event.bank,"commitment":"processed","block":parse_block(&frame)?})
    } else {
        capture(config, duration).await?
    };
    if let Some(path) = report {
        std::fs::write(path, serde_json::to_vec_pretty(&result)?)?;
    }
    println!("{}", result);
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = start().await {
        eprintln!("yield stream stopped: {error}");
        std::process::exit(1);
    }
}
