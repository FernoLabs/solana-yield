use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use yield_indexer::{
    config::{Config, SourceConfig},
    model::{Evidence, Observation, Origin, Rate, RateKind},
    service::{self, App},
    store::Store,
};

const NOW: u64 = 1_792_108_800_000;
const GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const RESERVE: &str = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp";
const MINT: &str = "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH";

fn native(id: &str) -> SourceConfig {
    SourceConfig::Kamino {
        id: id.into(),
        reserve: RESERVE.into(),
        mint: MINT.into(),
        asset: "USDG".into(),
        decimals: 6,
    }
}

fn api(id: &str) -> SourceConfig {
    SourceConfig::CatalogApi {
        id: id.into(),
        url_env: "YIELD_TEST_CATALOG_URL".into(),
    }
}

fn config(sources: Vec<SourceConfig>) -> Config {
    Config {
        listen: "127.0.0.1:8080".into(),
        database: ":memory:".into(),
        interval_seconds: 60,
        max_age_seconds: 180,
        allow_api_sources: true,
        rpc_url_env: Some("YIELD_TEST_RPC_URL".into()),
        genesis_hash: GENESIS.into(),
        sources,
        concurrency: 4,
        history_retention_days: 30,
        max_history_rows: 250_000,
    }
}

fn observation(source: &SourceConfig, id: &str, at: u64) -> Observation {
    let is_api = source.is_api();
    Observation {
        schema_version: 1,
        id: id.into(),
        source_id: source.id().into(),
        protocol: "lending".into(),
        protocols: vec!["lending".into()],
        market: "USDG reserve".into(),
        asset: "USDG".into(),
        denomination: "USDG".into(),
        method: if is_api {
            "provider_reported_apy"
        } else {
            "native_borrow_apr"
        }
        .into(),
        rates: vec![Rate {
            component: "borrow".into(),
            kind: if is_api { RateKind::Apy } else { RateKind::Apr },
            value: 0.05,
        }],
        window_seconds: None,
        observed_at_ms: Some(at),
        collected_at_ms: at,
        unavailable_reason: None,
        evidence: Evidence {
            origin: if is_api { Origin::Api } else { Origin::Onchain },
            transport: if is_api {
                "https_json"
            } else {
                "solana_json_rpc"
            }
            .into(),
            genesis_hash: (!is_api).then(|| GENESIS.into()),
            commitment: (!is_api).then(|| "finalized".into()),
            context_slot: (!is_api).then_some(42),
            accounts: if is_api {
                Vec::new()
            } else {
                vec![RESERVE.into(), MINT.into()]
            },
        },
    }
}

fn store(sources: Vec<SourceConfig>) -> Store {
    let mut store = Store::open(":memory:").unwrap();
    store.configure(&config(sources)).unwrap();
    store
}

fn rows(value: &Value) -> &[Value] {
    value["observations"].as_array().unwrap()
}

#[test]
fn a_failure_in_the_same_millisecond_cannot_erase_the_prior_good_history() {
    let source = api("reported");
    let mut db = store(vec![source.clone()]);
    db.ingest(&source, &[observation(&source, "market", NOW)], NOW)
        .unwrap();
    db.fail(&source, NOW).unwrap();
    let history = db.history("reported", "market", 10).unwrap();
    assert_eq!(rows(&history).len(), 2);
    assert!(rows(&history)[0]["rates"].as_array().unwrap().is_empty());
    assert_eq!(rows(&history)[1]["rates"][0]["value"], 0.05);
}

#[test]
fn reviewed_protocol_aliases_share_coverage_without_merging_unknown_issuers() {
    let source = api("reported");
    let mut db = store(vec![source.clone()]);
    let mut value = observation(&source, "one", NOW);
    value.protocol = "kamino-lend".into();
    value.protocols = vec!["kamino-lend".into(), "Kamino".into(), "OnRe".into()];
    db.ingest(&source, &[value], NOW).unwrap();
    let current = db.latest(NOW, 180000, None, 0, 10).unwrap();
    assert_eq!(rows(&current)[0]["protocol"], "Kamino");
    assert_eq!(
        db.coverage(NOW, 180000).unwrap()["protocols"],
        json!(["Kamino", "OnRe"])
    );
}

#[test]
fn complete_400_position_catalog_keeps_all_ids_and_40_protocols_across_pages() {
    // Synthetic scale case. This is not a count of deployed sources.
    let source = api("catalog");
    let payload = json!({"coverage":{"positions":400},"rows":(0..400).map(|i| json!({
        "id":format!("pool:{i}"),"venue":format!("P{}",i%40),"name":format!("Pool{i}"),
        "token":"USDG","apy_latest":0.05,"observed_at":NOW/1000
    })).collect::<Vec<_>>()});
    let observations =
        yield_indexer::sources::api::catalog::parse(&payload, "catalog", NOW).unwrap();
    let mut db = store(vec![source.clone()]);
    db.ingest(&source, &observations, NOW).unwrap();
    let coverage = db.coverage(NOW, 180000).unwrap();
    assert_eq!(coverage["positions"], 400);
    assert_eq!(coverage["observations"], 400);
    assert_eq!(coverage["api"], 400);
    assert_eq!(coverage["onchain"], 0);
    assert_eq!(coverage["protocols"].as_array().unwrap().len(), 40);
    let mut ids = std::collections::BTreeSet::new();
    for offset in [0, 200] {
        let page = db.latest(NOW, 180000, Some("api"), offset, 200).unwrap();
        assert_eq!(page["total"], 400);
        for row in rows(&page) {
            assert!(ids.insert(row["id"].as_str().unwrap().to_owned()));
        }
    }
    assert_eq!(ids.len(), 400);
}

#[test]
fn stale_reads_remove_current_rates_without_rewriting_history_or_source_time() {
    let source = native("chain");
    let mut db = store(vec![source.clone()]);
    let mut value = observation(&source, "market", NOW - 10_000);
    value.collected_at_ms = NOW;
    db.ingest(&source, &[value], NOW).unwrap();
    let exact = db.latest(NOW, 10_000, None, 0, 100).unwrap();
    assert!(rows(&exact)[0]["unavailable_reason"].is_null());
    let stale = db.latest(NOW + 1, 10_000, None, 0, 100).unwrap();
    assert_eq!(rows(&stale)[0]["rates"], json!([]));
    assert_eq!(rows(&stale)[0]["unavailable_reason"], "source_stale");
    assert_eq!(rows(&stale)[0]["observed_at_ms"], NOW - 10_000);
    let history = db.history("chain", "market", 100).unwrap();
    assert_eq!(rows(&history).len(), 1);
    assert_eq!(rows(&history)[0]["rates"][0]["value"], 0.05);
    assert!(rows(&history)[0]["unavailable_reason"].is_null());
    assert_eq!(db.coverage(NOW + 1, 10_000).unwrap()["ready"], false);
}

#[test]
fn collection_failure_clears_rates_but_retains_the_original_observation_time() {
    let source = api("catalog");
    let mut db = store(vec![source.clone()]);
    let mut value = observation(&source, "market", NOW - 5000);
    value.collected_at_ms = NOW;
    db.ingest(&source, &[value], NOW).unwrap();
    db.fail(&source, NOW + 2000).unwrap();
    let current = db.latest(NOW + 2000, 180_000, None, 0, 100).unwrap();
    assert_eq!(rows(&current)[0]["rates"], json!([]));
    assert_eq!(rows(&current)[0]["unavailable_reason"], "collection_failed");
    assert_eq!(rows(&current)[0]["observed_at_ms"], NOW - 5000);
    assert_eq!(rows(&current)[0]["collected_at_ms"], NOW + 2000);
    let history = db.history("catalog", "market", 100).unwrap();
    assert_eq!(rows(&history).len(), 2);
    assert_eq!(rows(&history)[0]["rates"], json!([]));
    assert_eq!(rows(&history)[1]["rates"][0]["value"], 0.05);
    assert_eq!(db.sources().unwrap()["sources"][0]["status"], "error");
    assert_eq!(db.coverage(NOW + 2000, 180_000).unwrap()["ready"], false);
}

#[test]
fn empty_complete_response_removes_current_rows_without_removing_old_history() {
    let source = api("catalog");
    let mut db = store(vec![source.clone()]);
    db.ingest(&source, &[observation(&source, "removed", NOW)], NOW)
        .unwrap();
    db.ingest(&source, &[], NOW + 1).unwrap();
    assert!(rows(&db.latest(NOW + 1, 180_000, None, 0, 100).unwrap()).is_empty());
    assert_eq!(
        rows(&db.history("catalog", "removed", 100).unwrap()).len(),
        1
    );
    assert_eq!(db.sources().unwrap()["sources"][0]["status"], "unavailable");
    assert_eq!(db.coverage(NOW + 1, 180_000).unwrap()["ready"], false);
}

#[test]
fn invalid_batches_cannot_partly_replace_current_rows_or_forge_authority() {
    let source = api("catalog");
    let mut db = store(vec![source.clone()]);
    db.ingest(&source, &[observation(&source, "old", NOW)], NOW)
        .unwrap();
    let good = observation(&source, "new", NOW + 1);
    let mut invalid = observation(&source, "invalid", NOW + 1);
    invalid.rates[0].value = f64::NAN;
    assert!(db
        .ingest(&source, &[good.clone(), invalid], NOW + 1)
        .is_err());
    let mut forged = observation(&native("catalog"), "forged", NOW + 1);
    assert!(forged.validate().is_ok());
    assert!(db
        .ingest(&source, std::slice::from_ref(&forged), NOW + 1)
        .is_err());
    forged.evidence = observation(&source, "forged", NOW + 1).evidence;
    forged.source_id = "different-source".into();
    assert!(db.ingest(&source, &[forged], NOW + 1).is_err());
    assert!(db.ingest(&source, &[good.clone(), good], NOW + 1).is_err());
    for collected_at in [NOW + 2, NOW - 60_001] {
        let invalid_time = observation(&source, "invalid-time", collected_at);
        assert!(db.ingest(&source, &[invalid_time], NOW + 1).is_err());
    }
    let current = db.latest(NOW + 1, 180_000, None, 0, 100).unwrap();
    assert_eq!(rows(&current).len(), 1);
    assert_eq!(rows(&current)[0]["id"], "old");
    assert_eq!(rows(&db.history("catalog", "old", 100).unwrap()).len(), 1);
    assert!(rows(&db.history("catalog", "new", 100).unwrap()).is_empty());
    assert!(rows(&db.history("catalog", "forged", 100).unwrap()).is_empty());
}

#[test]
fn restart_configuration_requires_new_current_evidence_and_preserves_history() {
    let source = native("source");
    let mut db = store(vec![source.clone()]);
    db.ingest(&source, &[observation(&source, "market", NOW)], NOW)
        .unwrap();
    db.configure(&config(vec![source.clone()])).unwrap();
    assert!(rows(&db.latest(NOW, 180_000, None, 0, 100).unwrap()).is_empty());
    assert_eq!(db.sources().unwrap()["sources"][0]["status"], "pending");
    assert_eq!(rows(&db.history("source", "market", 100).unwrap()).len(), 1);
    db.ingest(&source, &[observation(&source, "market", NOW + 1)], NOW + 1)
        .unwrap();
    db.configure(&config(vec![api("source")])).unwrap();
    assert!(rows(&db.latest(NOW + 1, 180_000, None, 0, 100).unwrap()).is_empty());
    assert_eq!(db.sources().unwrap()["sources"][0]["origin"], "api");
    let history = db.history("source", "market", 100).unwrap();
    assert_eq!(rows(&history).len(), 2);
    assert!(rows(&history)
        .iter()
        .all(|row| row["evidence"]["origin"] == "onchain"));
    assert_eq!(db.coverage(NOW + 1, 180_000).unwrap()["ready"], false);
}

#[test]
fn source_and_observation_identity_keep_api_and_native_comparisons_separate() {
    let chain = native("chain");
    let catalog = api("catalog");
    let mut db = store(vec![chain.clone(), catalog.clone()]);
    let chain_value = observation(&chain, "same-market", NOW);
    let mut catalog_value = observation(&catalog, "same-market", NOW);
    catalog_value.protocol = "aggregator".into();
    catalog_value.protocols = vec!["aggregator".into(), "lending".into(), "oracle".into()];
    catalog_value.rates[0].value = 0.06;
    db.ingest(&chain, &[chain_value], NOW).unwrap();
    db.ingest(&catalog, &[catalog_value], NOW).unwrap();
    let current = db.latest(NOW, 180_000, None, 0, 100).unwrap();
    assert_eq!(current["total"], 2);
    assert_eq!(
        rows(&db.history("chain", "same-market", 100).unwrap())[0]["rates"][0]["value"],
        0.05
    );
    assert_eq!(
        rows(&db.history("catalog", "same-market", 100).unwrap())[0]["rates"][0]["value"],
        0.06
    );
    let coverage = db.coverage(NOW, 180_000).unwrap();
    assert_eq!(coverage["ready"], true);
    assert_eq!(coverage["onchain"], 1);
    assert_eq!(coverage["api"], 1);
    assert_eq!(coverage["positions"], 1);
    assert_eq!(coverage["observations"], 2);
    assert_eq!(
        coverage["protocols"],
        json!(["aggregator", "lending", "oracle"])
    );
}

#[test]
fn model_rejects_missing_time_invalid_rates_and_false_protocol_lineage() {
    let source = native("chain");
    let valid = observation(&source, "market", NOW);
    valid.validate().unwrap();
    for case in 0..13 {
        let mut value = valid.clone();
        match case {
            0 => value.observed_at_ms = None,
            1 => value.observed_at_ms = Some(NOW + 30_001),
            2 => value.rates[0].value = f64::INFINITY,
            3 => {
                value.rates[0].kind = RateKind::Apy;
                value.rates[0].value = -1.01;
            }
            4 => value.evidence.commitment = Some("processed".into()),
            5 => value.evidence.accounts.clear(),
            6 => value.protocols.clear(),
            7 => value.protocols.push("lending".into()),
            8 => value.protocols = vec!["unrelated".into()],
            9 => value.evidence.origin = Origin::Api,
            10 => value.evidence.genesis_hash = Some("not-a-genesis-hash".into()),
            11 => value.evidence.accounts = vec!["not-an-account".into()],
            12 => value.rates.push(value.rates[0].clone()),
            _ => unreachable!(),
        }
        assert!(value.validate().is_err(), "invalid model case {case}");
    }
    let mut zero = valid.clone();
    zero.rates[0].value = 0.0;
    zero.validate().unwrap();
    let mut unknown = valid;
    unknown.rates.clear();
    assert!(unknown.validate().is_err());
    unknown.unavailable_reason = Some("unsupported rate basis".into());
    unknown.validate().unwrap();
}

#[test]
fn examples_validate_and_api_sources_need_explicit_permission() {
    let chain: Config =
        serde_json::from_str(include_str!("../examples/config.chain.json")).unwrap();
    chain.validate().unwrap();
    assert!(!chain.allow_api_sources);
    assert_eq!(chain.concurrency, 4);
    assert_eq!(chain.history_retention_days, 30);
    assert_eq!(chain.max_history_rows, 250_000);
    let mut api: Config =
        serde_json::from_str(include_str!("../examples/config.api.json")).unwrap();
    api.validate().unwrap();
    api.allow_api_sources = false;
    assert!(api.validate().is_err());
    let mut duplicate = chain.clone();
    duplicate.sources.push(duplicate.sources[0].clone());
    assert!(duplicate.validate().is_err());
    for limit in [0, 17] {
        let mut invalid = chain.clone();
        invalid.concurrency = limit;
        assert!(invalid.validate().is_err());
    }
    let mut unknown = serde_json::to_value(chain).unwrap();
    unknown["ignore_stale_rates"] = json!(true);
    assert!(serde_json::from_value::<Config>(unknown).is_err());
}

#[test]
fn retention_limits_history_without_erasing_the_latest_observation() {
    let source = api("catalog");
    let mut db = store(vec![source.clone()]);
    for at in [NOW - 2 * 86_400_000, NOW - 2000, NOW - 1000, NOW] {
        db.ingest(&source, &[observation(&source, "market", at)], at)
            .unwrap();
    }
    db.prune(NOW, 1, 2).unwrap();
    let history = db.history("catalog", "market", 100).unwrap();
    assert_eq!(rows(&history).len(), 2);
    assert_eq!(rows(&history)[0]["observed_at_ms"], NOW);
    assert_eq!(rows(&history)[1]["observed_at_ms"], NOW - 1000);
    assert_eq!(
        rows(&db.latest(NOW, 180_000, None, 0, 100).unwrap()).len(),
        1
    );
}

fn test_router() -> Router {
    let chain = native("chain");
    let catalog = api("api");
    let configuration = config(vec![chain.clone(), catalog.clone()]);
    let mut db = store(configuration.sources.clone());
    let at = service::now_ms();
    db.ingest(&chain, &[observation(&chain, "native-market", at)], at)
        .unwrap();
    db.ingest(
        &catalog,
        &[
            observation(&catalog, "alpha", at),
            observation(&catalog, "beta", at),
            observation(&catalog, "gamma", at),
        ],
        at,
    )
    .unwrap();
    service::router(App {
        config: Arc::new(configuration),
        store: Arc::new(Mutex::new(db)),
        http: reqwest::Client::new(),
        rpc: None,
    })
}

async fn get(app: &Router, url: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    (status, body)
}

#[tokio::test]
async fn http_filters_paginate_current_rows_and_scope_history_by_source() {
    let app = test_router();
    let (status, page) = get(&app, "/v1/yields?origin=api&offset=1&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 3);
    assert_eq!(page["offset"], 1);
    assert_eq!(rows(&page).len(), 1);
    assert_eq!(rows(&page)[0]["id"], "beta");
    let (_, native) = get(&app, "/v1/yields?origin=onchain").await;
    assert_eq!(native["total"], 1);
    assert_eq!(rows(&native)[0]["evidence"]["origin"], "onchain");
    let (status, history) = get(&app, "/v1/history?source_id=api&id=alpha&limit=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows(&history).len(), 1);
    assert_eq!(rows(&history)[0]["source_id"], "api");
    let (_, other_source) = get(&app, "/v1/history?source_id=chain&id=alpha").await;
    assert!(rows(&other_source).is_empty());
    let (_, health) = get(&app, "/health").await;
    assert_eq!(health["ok"], true);
    assert_eq!(health["ready"], true);
    let (_, coverage) = get(&app, "/v1/coverage").await;
    assert_eq!(coverage["positions"], 4);
    assert_eq!(coverage["onchain"], 1);
    assert_eq!(coverage["api"], 3);
    let (_, sources) = get(&app, "/v1/sources").await;
    assert_eq!(sources["sources"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn http_rejects_unbounded_or_ambiguous_queries() {
    let app = test_router();
    for url in [
        "/v1/yields?origin=native",
        "/v1/yields?limit=0",
        "/v1/yields?limit=1001",
        "/v1/yields?offset=1000001",
        "/v1/yields?unknown=true",
        "/v1/history?id=alpha",
        "/v1/history?source_id=api&id=alpha&limit=0",
        "/v1/history?source_id=api&id=alpha&limit=1001",
        "/v1/history?source_id=api&id=",
        "/v1/history?source_id=&id=alpha",
    ] {
        assert_eq!(
            get(&app, url).await.0,
            StatusCode::BAD_REQUEST,
            "query {url}"
        );
    }
}

#[test]
fn protocol_filter_matches_lineage_and_applies_before_pagination() {
    let source = api("catalog");
    let mut db = store(vec![source.clone()]);
    let values = [
        ("a-other", "Other", vec!["Other"]),
        ("b-kamino", "kamino-lend", vec!["kamino-lend"]),
        ("c-other", "Other", vec!["Other"]),
        ("d-vault", "Vault", vec!["Vault", "Kamino"]),
        ("e-jupiter", "Jupiter", vec!["Jupiter"]),
    ]
    .into_iter()
    .map(|(id, protocol, protocols)| {
        let mut value = observation(&source, id, NOW);
        value.protocol = protocol.into();
        value.protocols = protocols.into_iter().map(str::to_owned).collect();
        value
    })
    .collect::<Vec<_>>();
    db.ingest(&source, &values, NOW).unwrap();
    let page = db
        .latest_for_protocol(NOW, 180_000, Some("api"), Some("KAMINO"), 1, 1)
        .unwrap();
    assert_eq!(page["total"], 2);
    assert_eq!(rows(&page).len(), 1);
    assert_eq!(rows(&page)[0]["id"], "d-vault");
    let alias = db
        .latest_for_protocol(NOW, 180_000, None, Some("kamino-lend"), 0, 10)
        .unwrap();
    assert_eq!(alias["total"], 2);
    assert_eq!(
        db.latest_for_protocol(NOW, 180_000, Some("onchain"), Some("Kamino"), 0, 10)
            .unwrap()["total"],
        0
    );
}

#[tokio::test]
async fn http_protocol_filter_and_bundled_contract_are_available() {
    let app = test_router();
    let (status, page) = get(
        &app,
        "/v1/yields?protocol=LENDING&origin=api&offset=1&limit=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 3);
    assert_eq!(rows(&page)[0]["id"], "beta");
    assert_eq!(
        get(&app, "/v1/yields?protocol=").await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, registry) = get(&app, "/v1/protocols").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(registry["schema_version"], 1);
    let protocols = registry["protocols"].as_array().unwrap();
    assert!(protocols
        .iter()
        .any(|p| p["id"] == "kamino" && p["parser_status"] == "implemented"));
    assert!(protocols
        .iter()
        .any(|p| p["parser_status"] != "implemented"));
    let (status, specification) = get(&app, "/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(specification["openapi"], "3.1.0");
    assert!(specification["paths"]["/v1/yields"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "protocol"));
}

#[tokio::test]
async fn serve_only_requires_no_sources_and_keeps_saved_observations() {
    let mut configuration: Config =
        serde_json::from_str(include_str!("../examples/config.server.json")).unwrap();
    configuration.validate_server().unwrap();
    assert!(configuration.validate().is_err());
    let path = std::env::temp_dir().join(format!(
        "solana-yield-api-{}-{}.sqlite",
        std::process::id(),
        service::now_ms()
    ));
    configuration.database = path.to_str().unwrap().into();
    let source = api("saved-source");
    let mut saved = Store::open(&configuration.database).unwrap();
    saved.configure(&config(vec![source.clone()])).unwrap();
    let at = service::now_ms();
    saved
        .ingest(&source, &[observation(&source, "saved-market", at)], at)
        .unwrap();
    drop(saved);
    let app = App::server_only(configuration).unwrap();
    assert!(app.rpc.is_none());
    let router = service::router(app);
    let (status, page) = get(&router, "/v1/yields").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 1);
    assert_eq!(rows(&page)[0]["id"], "saved-market");
    let (_, sources) = get(&router, "/v1/sources").await;
    assert_eq!(sources["sources"][0]["id"], "saved-source");
    drop(router);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[tokio::test]
async fn empty_server_has_no_current_rate_and_is_not_ready() {
    let mut configuration: Config =
        serde_json::from_str(include_str!("../examples/config.server.json")).unwrap();
    configuration.database = ":memory:".into();
    let app = App::server_only(configuration).unwrap();
    assert!(app.rpc.is_none());
    let router = service::router(app);
    let (status, health) = get(&router, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["ok"], true);
    assert_eq!(health["ready"], false);
    let (_, current) = get(&router, "/v1/yields").await;
    assert_eq!(current["total"], 0);
    assert!(rows(&current).is_empty());
}
