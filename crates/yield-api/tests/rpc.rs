use axum::{extract::State, routing::post, Json, Router};
use base64::{prelude::BASE64_STANDARD, Engine};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use yield_indexer::rpc::{RpcClient, CLOCK};

const GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const KEY: &str = "11111111111111111111111111111111";

#[derive(Clone)]
struct Mock {
    genesis: &'static str,
    clock_seconds: i64,
    wrong_encoding: bool,
    calls: Arc<AtomicUsize>,
}
async fn reply(State(mock): State<Mock>, Json(request): Json<Value>) -> Json<Value> {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    let result = match request["method"].as_str().unwrap() {
        "getGenesisHash" => json!(mock.genesis),
        "getMultipleAccounts" => {
            assert_eq!(request["params"][1]["commitment"], "finalized");
            assert_eq!(request["params"][1]["encoding"], "base64");
            assert_eq!(request["params"][0], json!([KEY, CLOCK]));
            let mut clock = vec![0_u8; 40];
            clock[..8].copy_from_slice(&500000001_u64.to_le_bytes());
            clock[32..40].copy_from_slice(&mock.clock_seconds.to_le_bytes());
            json!({"context":{"slot":500000001},"value":[
                {"owner":KEY,"lamports":42,"executable":false,"data":[BASE64_STANDARD.encode([1,2,3]),if mock.wrong_encoding { "jsonParsed" } else { "base64" }]},
                {"owner":"Sysvar1111111111111111111111111111111111111","lamports":1,"executable":false,"data":[BASE64_STANDARD.encode(clock),"base64"]}
            ]})
        }
        _ => panic!("unexpected RPC method"),
    };
    Json(json!({"jsonrpc":"2.0","id":1,"result":result}))
}
async fn server(mock: Mock) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/", post(reply)).with_state(mock),
        )
        .await
        .unwrap();
    });
    (url, task)
}
fn mock() -> Mock {
    Mock {
        genesis: GENESIS,
        clock_seconds: (yield_indexer::service::now_ms() / 1000) as i64,
        wrong_encoding: false,
        calls: Arc::new(AtomicUsize::new(0)),
    }
}
fn client(url: &str) -> RpcClient {
    RpcClient::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap(),
        url,
        GENESIS,
    )
    .unwrap()
}

#[tokio::test]
async fn preserves_actual_context_and_chain_clock_and_checks_genesis_once() {
    let mock = mock();
    let time = mock.clock_seconds as u64 * 1000;
    let calls = mock.calls.clone();
    let (url, task) = server(mock).await;
    let client = client(&url);
    for _ in 0..2 {
        let snapshot = client.snapshot(&[KEY.into()]).await.unwrap();
        assert_eq!(snapshot.slot, 500000001);
        assert_eq!(snapshot.observed_at_ms, time);
        assert_eq!(snapshot.genesis_hash, GENESIS);
        assert_eq!(snapshot.get(KEY).unwrap().data, [1, 2, 3]);
        assert_eq!(snapshot.get(KEY).unwrap().lamports, 42);
        assert!(snapshot.get(CLOCK).is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    task.abort();
}

#[tokio::test]
async fn rejects_wrong_chain_before_any_account_read() {
    let mut mock = mock();
    mock.genesis = KEY;
    let calls = mock.calls.clone();
    let (url, task) = server(mock).await;
    assert!(client(&url)
        .snapshot(&[KEY.into()])
        .await
        .unwrap_err()
        .to_string()
        .contains("genesis"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn rejects_stale_clock_and_unknown_account_encoding() {
    for wrong_encoding in [false, true] {
        let mut mock = mock();
        mock.wrong_encoding = wrong_encoding;
        if !wrong_encoding {
            mock.clock_seconds -= 3600;
        }
        let (url, task) = server(mock).await;
        assert!(client(&url).snapshot(&[KEY.into()]).await.is_err());
        task.abort();
    }
}

#[test]
fn endpoint_requires_transport_security_without_revealing_credentials() {
    assert!(yield_indexer::rpc::endpoint("http://example.com?key=secret").is_err());
    let error = yield_indexer::rpc::endpoint("https://name:secret@example.com")
        .unwrap_err()
        .to_string();
    assert!(!error.contains("secret") && !error.contains("example.com"));
}

#[tokio::test]
async fn chain_collection_serves_a_computed_rate_then_invalidates_it_on_rpc_failure() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use std::sync::{atomic::AtomicBool, Mutex};
    use tower::ServiceExt;
    use yield_indexer::{
        config::Config,
        service::{router, App},
        store::Store,
    };
    const RESERVE: &str = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp";
    const MINT: &str = "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH";
    let failed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_failed = failed.clone();
    let handler_calls = calls.clone();
    let source=Router::new().route("/",post(move |Json(request):Json<Value>| {
        let failed=handler_failed.clone(); let calls=handler_calls.clone();
        async move {
            calls.fetch_add(1,Ordering::SeqCst);
            if request["method"]=="getGenesisHash" {return Json(json!({"jsonrpc":"2.0","id":1,"result":GENESIS}));}
            if failed.load(Ordering::SeqCst) {return Json(json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"mock-private-token"}}));}
            assert_eq!(request["method"],"getMultipleAccounts");
            assert_eq!(request["params"][0],json!([RESERVE,MINT,CLOCK]));
            assert_eq!(request["params"][1]["commitment"],"finalized");
            let seconds=(yield_indexer::service::now_ms()/1000) as u32;
            let mut reserve=BASE64_STANDARD.decode(include_str!("../fixtures/native/kamino-reserve-usdg-main.b64").trim()).unwrap();
            // Recorded rate inputs with synthetic current update time for this local transport test.
            reserve[16..24].copy_from_slice(&500000001_u64.to_le_bytes());
            reserve[28..32].copy_from_slice(&seconds.to_le_bytes());
            let mut mint=vec![0_u8;82]; mint[44]=6;mint[45]=1;
            let mut clock=vec![0_u8;40];clock[..8].copy_from_slice(&500000001_u64.to_le_bytes());clock[32..40].copy_from_slice(&i64::from(seconds).to_le_bytes());
            let account=|owner:&str,data:Vec<u8>| json!({"owner":owner,"lamports":1,"executable":false,"data":[BASE64_STANDARD.encode(data),"base64"]});
            Json(json!({"jsonrpc":"2.0","id":1,"result":{"context":{"slot":500000001},"value":[
                account("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD",reserve),account("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",mint),account("Sysvar1111111111111111111111111111111111111",clock)
            ]}}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, source).await.unwrap();
    });
    let mut config:Config=serde_json::from_value(json!({"listen":"127.0.0.1:0","database":":memory:","interval_seconds":60,"max_age_seconds":180,"allow_api_sources":false,"rpc_url_env":"UNUSED_TEST_RPC","genesis_hash":GENESIS,"sources":[{"adapter":"kamino","id":"native-test","reserve":RESERVE,"mint":MINT,"asset":"USDG","decimals":6}]})).unwrap();
    config.validate().unwrap();
    let mut store = Store::open(":memory:").unwrap();
    store.configure(&config).unwrap();
    let app = App {
        config: Arc::new(config.clone()),
        store: Arc::new(Mutex::new(store)),
        http: reqwest::Client::new(),
        rpc: Some(client(&url)),
    };
    let coverage = app.collect_once().await.unwrap();
    assert_eq!(coverage["ready"], true);
    assert_eq!(coverage["onchain"], 1);
    assert_eq!(coverage["api"], 0);
    let response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/v1/yields?origin=onchain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["observations"][0]["protocol"], "Kamino");
    assert!(
        (body["observations"][0]["rates"][0]["value"]
            .as_f64()
            .unwrap()
            - 0.054056439)
            .abs()
            < 1e-9
    );
    assert_eq!(
        body["observations"][0]["evidence"]["context_slot"],
        500000001_u64
    );
    failed.store(true, Ordering::SeqCst);
    assert_eq!(app.collect_once().await.unwrap()["ready"], false);
    let db = app.store.lock().unwrap();
    let latest = db
        .latest(yield_indexer::service::now_ms(), 180000, None, 0, 10)
        .unwrap();
    assert_eq!(latest["observations"][0]["rates"], json!([]));
    assert_eq!(
        latest["observations"][0]["unavailable_reason"],
        "collection_failed"
    );
    let history = db.history("native-test", "native-test", 10).unwrap();
    assert_eq!(history["observations"].as_array().unwrap().len(), 2);
    assert_eq!(history["observations"][1]["rates"][0]["kind"], "apr");
    assert!(!db
        .sources()
        .unwrap()
        .to_string()
        .contains("mock-private-token"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    config.sources.clear(); // no further collection after the mock is stopped
    task.abort();
}
