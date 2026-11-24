# Blockzilla bank client

This Rust library reads the opt-in `native-bank-compact-v2` backend feed. It
uses the shared strict v2 contract. It does not link Agave, Hivezilla relay
services, or a transaction decoder. The crate is private and is not on crates.io.

The public demo is a different, processed blockhash feed. Use a backend Bearer
key and an operator-supplied `SourceIdentity` for this client. Keep the key in a
private file or server secret. The client requires an HTTPS base URL, rejects URL
credentials and query parameters, and does not follow redirects.

## Application contract

```rust,ignore
use blockzilla_bank_client::{BankClient, ClientConfig, StreamEvent};

let client = BankClient::new(ClientConfig::new(url, private_key, pinned_source))?;
let bootstrap = client.bootstrap(saved_cursor.as_deref()).await?;

// First join: commit the bootstrap boundary and its exact selected bank.
// Resume: retain current-head dependencies as proofs. Do not apply the head
// control ahead of the events at the saved cursor.
commit_bootstrap_dependencies(&bootstrap).await?;
let mut stream = client.connect(bootstrap).await?;

loop {
    match stream.next().await? {
        StreamEvent::Status(head) => update_health(head),
        StreamEvent::Bank(delivery) => {
            let decoded = delivery.decode_with(my_portable_decoder)?;
            // One database transaction: bank data, control, application effects,
            // payload dependencies, and delivery.frame.cursor.
            stream.ack(&delivery.frame.cursor, || async {
                commit_bank_and_cursor(&delivery, decoded).await
            }).await?;
        }
    }
}
```

`bootstrap(None)` explicitly joins the current head. A saved cursor is checked
against the pinned source and replay range. It is never replaced with the head.
The client fetches and checks the exact head control and processed payload.
Commit those dependencies before `connect`. On a saved resume, current-head
proofs do not authorize application effects before ordered replay reaches them.

`next()` returns validated status or bank events in order. Up to four finite GETs
run in parallel by default, including missing control dependency GETs. The bound
can be set from one to sixteen. A background task keeps notice-frame downloads
active while the application applies a delivery. Its channel holds at most that
many items. A fixed 64 MiB budget covers all notice-frame bytes being fetched or
queued; each size is reserved before GET and also limits the response body.
The one consumer frame and its one missing dependency are outside that budget,
each bounded by the protocol frame limit. State changes and dependency lookup
remain ordered on the consumer. Strict notice admission occurs before each
credentialed GET. Every full-frame digest, source, cursor, length, and bank
identity is checked. A `Delivery` includes a missing processed dependency when
the client's metadata state does not already have that payload.

`Delivery::decode_with` is the decoder hook. It receives full frame bytes for the
event and any dependency. The application must check semantic compact data,
signatures, errors, and native block height before it commits or acknowledges.
Use a bounded worker pool for CPU-heavy decoding. The transport alone does not
certify transaction contents.

Only one bank can await acknowledgement. Reading again gives `AwaitingAck`.
An out-of-order ACK gives `AckOrder` and does not call the checkpoint hook. The
hook can perform the atomic application commit, or verify that the caller has
already made it. Only a successful hook advances `stream.cursor()`. A failed
hook stops the stream and preserves the old reconnect boundary. Hooks must be
safe to repeat if the caller cancels the future after a database commit.

`next()` is safe to cancel, including during a control dependency fetch or
reconnect. The stream owns those futures and resumes them on the next call. This
permits a separate timer for application readiness. Dropping the stream cancels
the producer and all pending reads. Recovery also cancels the old producer and
discards its queued frames before reopening from the acknowledged cursor.

## Readiness and recovery

`ready_hash(now_ms)` delegates to the shared `ClientState`. A processed block
cannot supply a confirmed hash. Readiness requires the exact confirmed/root
payload, current health, the applied provider head, and valid source and
selection ages. It is unavailable before the initial SSE status, during pending
application commits, after an I/O failure, or after `force_unavailable()` until a
new validated status arrives. `selected()` exposes the native observation; keep
its bank, hash, context slot, block height, expiry, and time together.

On resume, also require that this observation matches the application's durable
bank projection before serving it. The client's metadata is not an application
snapshot. Persist a fork-aware `Chain`, transaction outcomes, and application
undo separately. Do not write processed candidates into finalized history.

Transient connection/body failures and HTTP 429/5xx retry with bounded backoff.
Reconnect starts at the acknowledged cursor and drops speculative reads. The
default is eight retries, with a 250 ms initial delay and a five-second maximum
delay. Errors remain typed after exhaustion. `ClientError::is_transient()` and
`is_terminal()` let the supervisor choose its next action. Authentication,
source, digest, and protocol errors stop processing. A replay gap stops with a
`Gap` error and optional checked head; it never starts from a new boundary.

Archive recovery, automatic source handoff, complete account bytes, transaction
submission, and wallet watches belong to later or application-specific layers.

## Checks

From this directory:

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Tests use a local HTTP server with synthetic strict v2 frames. The public
constructor still requires HTTPS. Tests check header authentication, strict
source/notice/frame admission, bounded ordered reads, bootstrap dependencies,
retry/resume, explicit gaps, cancelled reads, durable ACK failure, and processed
hash rejection. These tests do not establish live provider or indexer capacity.
