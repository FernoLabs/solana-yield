use super::*;
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response as AxumResponse},
    routing::get,
};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use v2::fixtures::{block, control, source};

const KEY: &str = "test-backend-key-0000000000000000000000";
fn base_cursor() -> String {
    v2::cursor("fixture-session", 0, "-")
}
fn fresh(mut frame: Frame) -> Frame {
    frame.envelope.event.observed_at_unix_ms = now_ms();
    let length = u32::from_le_bytes(frame.wire[8..12].try_into().unwrap()) as usize;
    v2::encode(frame.envelope, &frame.wire[16 + length..]).unwrap()
}
fn head(frames: &[Frame], selected: Option<&Frame>) -> Head {
    let now = now_ms();
    let observation = selected.map(|f| ConfirmedObservation::from_frame(f).unwrap());
    Head {
        schema: v2::HEAD.into(),
        profile: v2::PROFILE.into(),
        source: source(),
        cursor: Some(frames.last().map_or_else(base_cursor, |f| f.cursor.clone())),
        earliest_after: Some(base_cursor()),
        status: if selected.is_some() {
            "live"
        } else {
            "starting"
        }
        .into(),
        sampled_at_unix_ms: now,
        source_observed_at_unix_ms: observation.as_ref().map(|c| c.observed_at_unix_ms),
        source_lag_ms: observation
            .as_ref()
            .map(|c| now.saturating_sub(c.observed_at_unix_ms)),
        latest_confirmed: observation,
    }
}
#[derive(Default)]
struct Modes {
    wrong_start: bool,
    gap: bool,
    bad_notice: bool,
    wrong_source: bool,
    corrupt_chunk: bool,
    first_connection_one_bank: bool,
    bootstrap_selected: bool,
    http_gap: bool,
    source_failed: bool,
    short_notice: bool,
}
struct ServerState {
    frames: Vec<Frame>,
    modes: Modes,
    chunk_hits: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    first_chunk_failures: AtomicUsize,
    delays: BTreeMap<String, Duration>,
    connections: Mutex<Vec<String>>,
    completed_fetches: Mutex<Vec<u64>>,
}
struct TestServer {
    client: BankClient,
    state: Arc<ServerState>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(
    frames: Vec<Frame>,
    modes: Modes,
    delays: BTreeMap<String, Duration>,
    failures: usize,
) -> TestServer {
    let state = Arc::new(ServerState {
        frames,
        modes,
        chunk_hits: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        max_active: AtomicUsize::new(0),
        first_chunk_failures: AtomicUsize::new(failures),
        delays,
        connections: Mutex::new(vec![]),
        completed_fetches: Mutex::new(vec![]),
    });
    let router = Router::new()
        .route("/v2/banks/head", get(get_head))
        .route("/v2/banks/events", get(get_events))
        .route("/v2/banks/chunks/{sha}", get(get_chunk))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = ClientConfig::new(
        format!("http://{}/", listener.local_addr().unwrap()),
        KEY,
        source(),
    );
    config.retry = RetryPolicy {
        max_retries: 2,
        initial_delay: Duration::from_millis(2),
        max_delay: Duration::from_millis(4),
    };
    config.max_in_flight = 2;
    let client = BankClient::build(config, true).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    TestServer {
        client,
        state,
        task,
    }
}
fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|h| h.to_str().ok()) == Some(&format!("Bearer {KEY}"))
}
fn selected(state: &ServerState) -> Option<&Frame> {
    state
        .frames
        .iter()
        .rev()
        .find(|f| matches!(f.envelope.event.kind.as_str(), "confirmed" | "root"))
}
async fn get_head(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> AxumResponse {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut h = head(
        &state.frames,
        state
            .modes
            .bootstrap_selected
            .then(|| selected(&state))
            .flatten(),
    );
    if state.modes.wrong_source {
        h.source.session_id = "unexpected-session".into();
    }
    if state.modes.source_failed {
        h.status = "failed".into();
    }
    axum::Json(h).into_response()
}
#[derive(serde::Deserialize)]
struct Position {
    after: String,
}
async fn get_events(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(position): Query<Position>,
) -> AxumResponse {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let connection = {
        let mut connections = state.connections.lock().unwrap();
        connections.push(position.after.clone());
        connections.len()
    };
    let h = head(&state.frames, selected(&state));
    if state.modes.http_gap {
        return (
            StatusCode::GONE,
            axum::Json(serde_json::json!({"error":"replay_unavailable","head":h})),
        )
            .into_response();
    }
    let mut events = format!(
        "event: status\r\ndata: {}\r\n\r\n",
        serde_json::to_string(&h).unwrap()
    );
    if state.modes.gap {
        events.push_str(&format!(
            "event: gap\ndata: {}\n\n",
            serde_json::json!({"error":"replay_unavailable","head":h})
        ));
    } else {
        let after = v2::parse_cursor(&position.after).unwrap().1;
        for f in &state.frames {
            if f.envelope.event.sequence <= after {
                continue;
            }
            let mut notice = v2::Notice::from_frame(f);
            if state.modes.bad_notice {
                notice.url = "https://unexpected.invalid/private".into();
            }
            if state.modes.short_notice {
                notice.bytes = 17;
            }
            events.push_str(&format!(
                "event: chunk\nid: {}\ndata: {}\n\n",
                f.cursor,
                serde_json::to_string(&notice).unwrap()
            ));
            if state.modes.first_connection_one_bank && connection == 1 {
                break;
            }
        }
    }
    let boundary = if state.modes.wrong_start {
        "wrong-boundary"
    } else {
        &position.after
    };
    (
        [
            ("content-type", "text/event-stream"),
            ("x-blockzilla-start-cursor", boundary),
        ],
        Body::from(events),
    )
        .into_response()
}
async fn get_chunk(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(sha): Path<String>,
) -> AxumResponse {
    if !authorized(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    state.chunk_hits.fetch_add(1, Ordering::SeqCst);
    if state
        .first_chunk_failures
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
        .is_ok()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(frame) = state.frames.iter().find(|f| f.sha256 == sha) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_active.fetch_max(active, Ordering::SeqCst);
    if let Some(delay) = state.delays.get(&sha) {
        tokio::time::sleep(*delay).await;
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
    state
        .completed_fetches
        .lock()
        .unwrap()
        .push(frame.envelope.event.sequence);
    let bytes = if state.modes.corrupt_chunk {
        vec![0; frame.wire.len()]
    } else {
        frame.wire.to_vec()
    };
    (
        [("content-type", "application/vnd.blockzilla.stream.v2")],
        Body::from(bytes),
    )
        .into_response()
}
async fn bank(stream: &mut BankStream) -> Delivery {
    loop {
        match stream.next().await.unwrap() {
            StreamEvent::Status(_) => {}
            StreamEvent::Bank(delivery) => return delivery,
        }
    }
}
async fn acknowledge(stream: &mut BankStream, delivery: &Delivery) {
    stream
        .ack(&delivery.frame.cursor, || async {
            Ok::<_, std::convert::Infallible>(())
        })
        .await
        .unwrap();
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("mock condition timed out");
}

fn many_blocks(count: u64) -> Vec<Frame> {
    (1..=count)
        .map(|sequence| {
            fresh(block(
                sequence,
                20 + sequence,
                100 + sequence,
                19 + sequence,
                sequence,
            ))
        })
        .collect()
}

#[tokio::test]
async fn background_prefetch_refills_while_delivery_is_unacknowledged() {
    let frames = many_blocks(12);
    let delayed = frames[1].sha256.clone();
    let test = server(
        frames,
        Modes::default(),
        BTreeMap::from([(delayed, Duration::from_millis(80))]),
        0,
    )
    .await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
        .await
        .unwrap();
    let first = bank(&mut stream).await;
    assert_eq!(first.frame.envelope.event.sequence, 1);
    // Initial buffered reads are only two. Fetching five proves that the
    // producer continued and refilled without another consumer poll or ACK.
    wait_until(|| test.state.chunk_hits.load(Ordering::SeqCst) >= 5).await;
    assert_eq!(stream.cursor(), base_cursor());
    assert!(stream.selected().is_none());
    assert!(stream.ready_hash(now_ms()).is_err());
    assert!(matches!(stream.next().await, Err(ClientError::AwaitingAck)));
    let fetched = test.state.chunk_hits.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    // One delivered frame, channel capacity two, up to two buffered futures,
    // and one result blocked on channel send form a finite count bound.
    assert!(test.state.chunk_hits.load(Ordering::SeqCst) <= 6);
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), fetched);
    let committed = AtomicUsize::new(0);
    stream
        .ack(&first.frame.cursor, || async {
            committed.store(1, Ordering::SeqCst);
            Ok::<_, std::convert::Infallible>(())
        })
        .await
        .unwrap();
    assert_eq!(committed.load(Ordering::SeqCst), 1);
    assert_eq!(stream.cursor(), first.frame.cursor);
    for expected in 2..=6 {
        let delivery = bank(&mut stream).await;
        assert_eq!(delivery.frame.envelope.event.sequence, expected);
        acknowledge(&mut stream, &delivery).await;
    }
}

#[tokio::test]
async fn queued_byte_budget_stops_gets_and_drop_aborts_the_producer() {
    let test = server(many_blocks(12), Modes::default(), BTreeMap::new(), 0).await;
    let budget_size = test.state.frames[0].wire.len() * 2;
    let budget = Arc::new(Semaphore::new(budget_size));
    let mut incoming = test
        .client
        .open_bounded(&base_cursor(), budget.clone())
        .await
        .unwrap();
    let producer = incoming.task.abort_handle();
    wait_until(|| test.state.chunk_hits.load(Ordering::SeqCst) == 2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), 2);
    assert_eq!(budget.available_permits(), 0);
    assert!(matches!(
        incoming.next().await.unwrap().unwrap(),
        Prepared::Status(_)
    ));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), 2);
    assert!(matches!(
        incoming.next().await.unwrap().unwrap(),
        Prepared::Bank(_)
    ));
    wait_until(|| test.state.chunk_hits.load(Ordering::SeqCst) == 3).await;
    drop(incoming);
    wait_until(|| producer.is_finished() && budget.available_permits() == budget_size).await;
    let fetched = test.state.chunk_hits.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), fetched);
    assert_eq!(test.state.connections.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn notice_reservation_also_limits_the_response_body() {
    let test = server(
        many_blocks(1),
        Modes {
            short_notice: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    let budget = Arc::new(Semaphore::new(MAX_PREPARED_BYTES));
    let mut incoming = test
        .client
        .open_bounded(&base_cursor(), budget.clone())
        .await
        .unwrap();
    assert!(matches!(
        incoming.next().await.unwrap().unwrap(),
        Prepared::Status(_)
    ));
    assert!(matches!(
        incoming.next().await.unwrap(),
        Err(ClientError::Protocol(_))
    ));
    wait_until(|| budget.available_permits() == MAX_PREPARED_BYTES).await;
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), 1);
}

#[test]
fn requires_https_pinned_source_and_valid_bounds() {
    let config = ClientConfig::new("http://127.0.0.1/", KEY, source());
    assert!(matches!(
        BankClient::new(config),
        Err(ClientError::Configuration(_))
    ));
    let mut config = ClientConfig::new("https://example.invalid/?api_key=ignored", KEY, source());
    assert!(BankClient::new(config.clone()).is_err());
    config.url = "https://example.invalid/".into();
    config.max_in_flight = 0;
    assert!(BankClient::new(config.clone()).is_err());
    config.max_in_flight = 4;
    config.api_key.push('\n');
    assert!(BankClient::new(config).is_err());
}

#[tokio::test]
async fn preserves_order_bounded_reads_and_durable_ack() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let c = control(&b, 2, "confirmed", now_ms());
    let delays = BTreeMap::from([(b.sha256.clone(), Duration::from_millis(80))]);
    let test = server(vec![b.clone(), c.clone()], Modes::default(), delays, 0).await;
    let bootstrap = test.client.bootstrap(Some(&base_cursor())).await.unwrap();
    let mut stream = test.client.connect(bootstrap).await.unwrap();
    let first = bank(&mut stream).await;
    assert_eq!(first.frame.cursor, b.cursor);
    assert_eq!(stream.cursor(), base_cursor());
    assert!(matches!(stream.next().await, Err(ClientError::AwaitingAck)));
    assert!(stream.ready_hash(now_ms()).is_err());
    let called = AtomicUsize::new(0);
    assert!(matches!(
        stream
            .ack(&c.cursor, || async {
                called.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &str>(())
            })
            .await,
        Err(ClientError::AckOrder)
    ));
    assert_eq!(called.load(Ordering::SeqCst), 0);
    acknowledge(&mut stream, &first).await;
    let second = bank(&mut stream).await;
    assert_eq!(second.frame.cursor, c.cursor);
    assert!(second.dependency.is_none());
    assert!(stream.ready_hash(now_ms()).is_err());
    acknowledge(&mut stream, &second).await;
    assert_eq!(
        stream.ready_hash(now_ms()).unwrap().last_valid_block_height,
        160
    );
    assert!(test.state.max_active.load(Ordering::SeqCst) <= 2);
    assert_eq!(*test.state.completed_fetches.lock().unwrap(), vec![2, 1]);
}

#[tokio::test]
async fn checkpoint_failure_preserves_resume_and_stops_readiness() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let test = server(vec![b], Modes::default(), BTreeMap::new(), 0).await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
        .await
        .unwrap();
    let delivery = bank(&mut stream).await;
    assert!(matches!(
        stream
            .ack(&delivery.frame.cursor, || async {
                Err::<(), _>("disk full")
            })
            .await,
        Err(ClientError::Checkpoint(_))
    ));
    assert_eq!(stream.cursor(), base_cursor());
    assert!(matches!(stream.next().await, Err(ClientError::Stopped)));
    assert!(stream.ready_hash(now_ms()).is_err());
}

#[tokio::test]
async fn reconnects_from_acknowledged_cursor_and_retries_503() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let c = control(&b, 2, "confirmed", now_ms());
    let test = server(
        vec![b.clone(), c.clone()],
        Modes {
            first_connection_one_bank: true,
            ..Default::default()
        },
        BTreeMap::new(),
        1,
    )
    .await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
        .await
        .unwrap();
    let first = bank(&mut stream).await;
    acknowledge(&mut stream, &first).await;
    let second = bank(&mut stream).await;
    assert_eq!(second.frame.cursor, c.cursor);
    assert_eq!(
        *test.state.connections.lock().unwrap(),
        vec![base_cursor(), b.cursor]
    );
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn validates_start_boundary_source_and_expired_resume() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let test = server(
        vec![b.clone()],
        Modes {
            wrong_start: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    assert!(matches!(
        test.client
            .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
            .await,
        Err(ClientError::Protocol(_))
    ));
    let test = server(
        vec![b],
        Modes {
            wrong_source: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    assert!(matches!(
        test.client.bootstrap(Some(&base_cursor())).await,
        Err(ClientError::Source(_))
    ));
    let test = server(vec![], Modes::default(), BTreeMap::new(), 0).await;
    assert!(matches!(
        test.client.bootstrap(Some("other:p0:-")).await,
        Err(ClientError::Source(_))
    ));
}

#[tokio::test]
async fn sse_gap_is_terminal_and_never_resets_boundary() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let test = server(
        vec![b],
        Modes {
            gap: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await.unwrap(),
        StreamEvent::Status(_)
    ));
    let error = stream.next().await.err().unwrap();
    assert!(matches!(error, ClientError::Gap { .. }));
    assert!(error.is_terminal());
    assert_eq!(stream.cursor(), base_cursor());
    assert_eq!(test.state.connections.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn http_gap_authentication_and_stopped_source_are_terminal() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let test = server(
        vec![b.clone()],
        Modes {
            http_gap: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    assert!(matches!(
        test.client
            .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
            .await,
        Err(ClientError::Gap { .. })
    ));
    assert_eq!(test.state.connections.lock().unwrap().len(), 1);
    let test = server(
        vec![b.clone()],
        Modes {
            source_failed: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    assert!(matches!(
        test.client.bootstrap(Some(&base_cursor())).await,
        Err(ClientError::Source(_))
    ));
    let test = server(vec![b], Modes::default(), BTreeMap::new(), 0).await;
    let mut wrong = (*test.client.config).clone();
    wrong.api_key = "incorrect-key-0000000000000000000000000".into();
    let client = BankClient::build(wrong, true).unwrap();
    assert!(matches!(
        client.head().await,
        Err(ClientError::Authentication(401))
    ));
}

#[tokio::test]
async fn cancelled_checkpoint_keeps_pending_bank_and_old_boundary() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let test = server(vec![b], Modes::default(), BTreeMap::new(), 0).await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
        .await
        .unwrap();
    let delivery = bank(&mut stream).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(2),
            stream.ack(&delivery.frame.cursor, || async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<_, std::convert::Infallible>(())
            })
        )
        .await
        .is_err()
    );
    assert_eq!(stream.cursor(), base_cursor());
    assert!(matches!(stream.next().await, Err(ClientError::AwaitingAck)));
    acknowledge(&mut stream, &delivery).await;
    assert_eq!(stream.cursor(), delivery.frame.cursor);
}

#[tokio::test]
async fn rejects_notice_before_credentialed_fetch_and_checks_digest() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let test = server(
        vec![b.clone()],
        Modes {
            bad_notice: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&base_cursor())).await.unwrap())
        .await
        .unwrap();
    stream.next().await.unwrap();
    assert!(matches!(stream.next().await, Err(ClientError::Protocol(_))));
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), 0);
    let test = server(
        vec![b.clone()],
        Modes {
            corrupt_chunk: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    assert!(matches!(
        test.client.fetch(&b.sha256).await,
        Err(ClientError::Digest(_))
    ));
}

#[tokio::test]
async fn resolves_exact_bootstrap_and_fetches_missing_control_dependency() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let c = control(&b, 2, "confirmed", now_ms());
    let test = server(
        vec![b.clone(), c.clone()],
        Modes {
            bootstrap_selected: true,
            ..Default::default()
        },
        BTreeMap::new(),
        0,
    )
    .await;
    let bootstrap = test.client.bootstrap(None).await.unwrap();
    assert_eq!(bootstrap.after, c.cursor);
    assert_eq!(bootstrap.control.as_ref().unwrap().sha256, c.sha256);
    assert_eq!(bootstrap.payload.as_ref().unwrap().sha256, b.sha256);
    let mut stream = test.client.connect(bootstrap).await.unwrap();
    assert!(stream.ready_hash(now_ms()).is_err());
    assert!(matches!(
        stream.next().await.unwrap(),
        StreamEvent::Status(_)
    ));
    assert!(stream.ready_hash(now_ms()).is_ok());
    stream.force_unavailable();
    assert!(stream.ready_hash(now_ms()).is_err());

    let test = server(
        vec![b.clone(), c.clone()],
        Modes::default(),
        BTreeMap::new(),
        0,
    )
    .await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&b.cursor)).await.unwrap())
        .await
        .unwrap();
    let delivery = bank(&mut stream).await;
    assert_eq!(delivery.dependency.as_ref().unwrap().sha256, b.sha256);
    let decoded = delivery
        .decode_with(|wire| v2::decode(wire.to_vec().into()).map(|f| f.envelope.event.sequence))
        .unwrap();
    assert_eq!(decoded.frame, 2);
    assert_eq!(decoded.dependency, Some(1));
}

#[tokio::test]
async fn cancelled_next_retains_control_and_dependency_fetch() {
    let b = fresh(block(1, 17, 7, 3, 10));
    let c = control(&b, 2, "confirmed", now_ms());
    let delays = BTreeMap::from([(b.sha256.clone(), Duration::from_millis(80))]);
    let test = server(vec![b.clone(), c.clone()], Modes::default(), delays, 0).await;
    let mut stream = test
        .client
        .connect(test.client.bootstrap(Some(&b.cursor)).await.unwrap())
        .await
        .unwrap();
    stream.next().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), stream.next())
            .await
            .is_err()
    );
    assert_eq!(stream.cursor(), b.cursor);
    let delivery = bank(&mut stream).await;
    assert_eq!(delivery.frame.cursor, c.cursor);
    assert_eq!(delivery.dependency.as_ref().unwrap().cursor, b.cursor);
    assert_eq!(test.state.chunk_hits.load(Ordering::SeqCst), 2);
}

#[test]
fn parser_bounds_and_crlf_multiline_fields() {
    let mut sse = Sse::default();
    for line in ["event: status\r", "data: first\r", "data: second\r"] {
        assert!(sse.line(line.as_bytes()).unwrap().is_none());
    }
    assert_eq!(
        sse.line(b"\r").unwrap().unwrap(),
        ("status".into(), "".into(), "first\nsecond".into())
    );
    sse.line(b"id:").unwrap();
    assert!(sse.line(b"id: duplicate").is_err());
    assert!(Sse::default().line(&vec![b'x'; v2::MAX_META]).is_err());
}
