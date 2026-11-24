use crate::{
    config::Config,
    rpc::RpcClient,
    sources::{self, Context},
    store::Store,
};
use anyhow::{anyhow, ensure, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use futures::{stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: Arc<Mutex<Store>>,
    pub http: reqwest::Client,
    pub rpc: Option<RpcClient>,
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        Self::with_collection(config, true)
    }

    pub fn server_only(config: Config) -> Result<Self> {
        Self::with_collection(config, false)
    }

    fn with_collection(config: Config, collect: bool) -> Result<Self> {
        if collect {
            config.validate()?;
        } else {
            config.validate_server()?;
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("yield-indexer/0.1.0")
            .build()?;
        let rpc = if collect && config.sources.iter().any(|source| !source.is_api()) {
            let name = config
                .rpc_url_env
                .as_ref()
                .ok_or_else(|| anyhow!("native sources require RPC configuration"))?;
            let value = std::env::var(name)
                .map_err(|_| anyhow!("configured RPC environment variable is missing"))?;
            Some(RpcClient::new(http.clone(), &value, &config.genesis_hash)?)
        } else {
            None
        };
        let mut store = Store::open(&config.database)?;
        if collect {
            store.configure(&config)?;
        }
        Ok(Self {
            config: Arc::new(config),
            store: Arc::new(Mutex::new(store)),
            http,
            rpc,
        })
    }

    pub async fn collect_once(&self) -> Result<Value> {
        let work = stream::iter(self.config.sources.iter().cloned())
            .map(|source| {
                let app = self.clone();
                async move {
                    let context = Context {
                        http: &app.http,
                        rpc: app.rpc.as_ref(),
                        collected_at_ms: now_ms(),
                    };
                    let result = tokio::time::timeout(
                        Duration::from_secs(30),
                        sources::collect(&source, &context),
                    )
                    .await;
                    let store = app.store.clone();
                    tokio::task::spawn_blocking(move || {
                        let mut db = store.lock().map_err(|_| anyhow!("database lock failed"))?;
                        let at = now_ms();
                        let saved = match result {
                            Ok(Ok(values)) => db.ingest(&source, &values, at),
                            _ => Err(anyhow!("collection_failed")),
                        };
                        if saved.is_err() {
                            eprintln!(
                                "yield source {} failed; current rates are unavailable",
                                source.id()
                            );
                            db.fail(&source, at)?;
                        }
                        Ok::<_, anyhow::Error>(())
                    })
                    .await
                    .map_err(|_| anyhow!("database worker failed"))?
                }
            })
            .buffer_unordered(self.config.concurrency);
        futures::pin_mut!(work);
        while let Some(result) = work.next().await {
            result?;
        }
        let app = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = app
                .store
                .lock()
                .map_err(|_| anyhow!("database lock failed"))?;
            db.prune(
                now_ms(),
                app.config.history_retention_days,
                app.config.max_history_rows,
            )?;
            db.coverage(now_ms(), app.config.max_age_seconds * 1000)
        })
        .await
        .map_err(|_| anyhow!("database worker failed"))?
    }
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/v1/protocols", get(protocols))
        .route("/v1/sources", get(source_list))
        .route("/v1/yields", get(yields))
        .route("/v1/history", get(history))
        .route("/v1/coverage", get(coverage))
        .layer(tower::limit::ConcurrencyLimitLayer::new(32))
        .with_state(app)
}

type ApiResult = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;
fn failure() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error":"database_unavailable"})),
    )
}
fn invalid() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":"invalid_query"})),
    )
}

async fn db_read(
    app: App,
    read: impl FnOnce(&Store) -> Result<Value> + Send + 'static,
) -> ApiResult {
    tokio::task::spawn_blocking(move || {
        let db = app
            .store
            .lock()
            .map_err(|_| anyhow!("database lock failed"))?;
        read(&db)
    })
    .await
    .map_err(|_| failure())?
    .map(Json)
    .map_err(|_| failure())
}

async fn protocols() -> ApiResult {
    serde_json::from_str(include_str!("../../../config/protocols.json"))
        .map(Json)
        .map_err(|_| failure())
}

async fn openapi() -> ApiResult {
    serde_json::from_str(include_str!("../../../docs/openapi.json"))
        .map(Json)
        .map_err(|_| failure())
}

async fn health(State(app): State<App>) -> ApiResult {
    let age = app.config.max_age_seconds * 1000;
    db_read(app, move |db| {
        let coverage = db.coverage(now_ms(), age)?;
        Ok(json!({"ok":true,"ready":coverage["ready"],"schema_version":1,"coverage":coverage}))
    })
    .await
}
async fn source_list(State(app): State<App>) -> ApiResult {
    db_read(app, |db| db.sources()).await
}
async fn coverage(State(app): State<App>) -> ApiResult {
    let age = app.config.max_age_seconds * 1000;
    db_read(app, move |db| db.coverage(now_ms(), age)).await
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct YieldQuery {
    origin: Option<String>,
    protocol: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}
async fn yields(State(app): State<App>, Query(query): Query<YieldQuery>) -> ApiResult {
    if query
        .origin
        .as_ref()
        .is_some_and(|v| v != "onchain" && v != "api")
        || query.protocol.as_ref().is_some_and(|v| {
            v.trim().is_empty() || v.len() > 512 || v.chars().any(char::is_control)
        })
        || query.offset.unwrap_or(0) > 1_000_000
        || query.limit.is_some_and(|v| !(1..=1000).contains(&v))
    {
        return Err(invalid());
    }
    let age = app.config.max_age_seconds * 1000;
    db_read(app, move |db| {
        db.latest_for_protocol(
            now_ms(),
            age,
            query.origin.as_deref(),
            query.protocol.as_deref(),
            query.offset.unwrap_or(0),
            query.limit.unwrap_or(200),
        )
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    source_id: String,
    id: String,
    limit: Option<usize>,
}
async fn history(State(app): State<App>, Query(query): Query<HistoryQuery>) -> ApiResult {
    if query.id.is_empty()
        || query.id.len() > 512
        || query.source_id.is_empty()
        || query.source_id.len() > 80
        || query.limit.is_some_and(|v| !(1..=1000).contains(&v))
    {
        return Err(invalid());
    }
    db_read(app, move |db| {
        db.history(&query.source_id, &query.id, query.limit.unwrap_or(100))
    })
    .await
}

pub async fn run(config: Config, once: bool) -> Result<()> {
    run_mode(config, once, false).await
}

pub async fn run_mode(config: Config, once: bool, serve_only: bool) -> Result<()> {
    ensure!(
        !(once && serve_only),
        "--once and --serve-only cannot be combined"
    );
    let app = if serve_only {
        App::server_only(config)?
    } else {
        App::new(config)?
    };
    if once {
        let report = app.collect_once().await?;
        println!("{}", report);
        ensure!(
            report["ready"] == true,
            "one or more sources have no current rate"
        );
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(&app.config.listen).await?;
    println!("yield indexer listening on {}", listener.local_addr()?);
    let task_app = app.clone();
    let task = (!serve_only).then(|| {
        tokio::spawn(async move {
            let mut timer =
                tokio::time::interval(Duration::from_secs(task_app.config.interval_seconds));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                timer.tick().await;
                if task_app.collect_once().await.is_err() {
                    eprintln!("yield collection cycle failed");
                }
            }
        })
    });
    let result = axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await;
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
    Ok(result?)
}
