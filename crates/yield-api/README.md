# Yield API

This Rust service stores yield observations in SQLite and serves a read-only
HTTP API. Each observation keeps its source, time, method, protocol lineage,
and rate type. A missing or stale value has an explicit unavailable reason.

## Run without collectors

From the repository root:

```sh
cargo run --locked -p yield-indexer -- \
  --config crates/yield-api/examples/config.server.json --serve-only
curl --fail http://127.0.0.1:8080/health
curl --fail http://127.0.0.1:8080/v1/protocols
curl --fail http://127.0.0.1:8080/openapi.json
```

`--serve-only` starts no source collectors and creates no RPC client. It serves
saved observations from the configured database. A new database is empty and
reports `ready: false`. Saved rates still pass the current freshness check; a
restart does not renew their source time. The server example uses a five-second
maximum age. It does not convert older data into a current observation.

The HTTP interface has no write routes or built-in authentication. The example
binds to `127.0.0.1`. See the [API guide](../../docs/api.md) and
[OpenAPI document](../../docs/openapi.json).

## Source collectors

Collection is an explicit mode: omit `--serve-only` and supply one or more
sources. `--once` collects once and returns a nonzero status when a configured
source has no current rate.

| Adapter | Source | Output |
| --- | --- | --- |
| `kamino` | Finalized accounts through configured JSON-RPC | Borrow APR for the supported reserve and True APR basis |
| `jupiter` | Finalized accounts through configured JSON-RPC | Borrow APR for the supported reserve and version 2 rate model |
| `catalog_api` | Configured HTTPS catalogue | Provider observations with original source times |
| `defillama` | DefiLlama pool API | Provider observations when original observation time is present |

The RPC collectors are optional. They are not part of the zero-RPC Blockzilla
proof of concept. To run a collector, set endpoint credentials in environment
variables and use a private copy of the corresponding example configuration.

```sh
export YIELD_RPC_URL='https://your-solana-rpc.example'
cargo run --locked -p yield-indexer -- \
  --config crates/yield-api/examples/config.chain.json --once
```

API collectors also need `allow_api_sources: true`. The example uses an
explicit catalogue URL environment variable:

```sh
export YIELD_CATALOG_URL='https://your-catalog.example/yields'
cargo run --locked -p yield-indexer -- \
  --config crates/yield-api/examples/config.api.json
```

The catalogue accepts a JSON array or an object with a `rows` array. Each row
needs a unique `id`, `venue`, `name`, `token`, `apy_latest` in fraction units,
and an original `observed_at` in Unix seconds or `observed_at_ms` in Unix
milliseconds. Missing source times and invalid rates remain unavailable.
A `first_paint` response is rejected. If `coverage.positions` is present, it
must equal the number of rows. A download time does not replace a source time.

A rate of `0.05` means 5%. APR, APY, borrowing costs, and rewards remain separate.
The service does not silently compound APR or add components. Compare values
only when their denomination, method, component, and period agree.

## Blockzilla inputs

The [protocol parser](../yield-parser/README.md) reads verified account bytes
and ledger records. The [stream tool](../yield-stream/README.md) saves
transaction evidence and Bank controls in its own database. The
[account observer](../../scripts/live-yield-shadow.py) checks native account
frames against a pinned source and a five-second freshness limit.

Those inputs do not yet populate this API's observation tables. Connecting the
verified account monitor, stored time series, and calculated returns is listed
in the [roadmap](../../docs/roadmap.md). The API protocol registry describes
implemented and planned parsers; it is not proof of current live coverage.

## Configuration

The examples are in [examples](examples). Unknown fields are rejected.

| Field | Meaning |
| --- | --- |
| `listen` | HTTP address; use a local address unless exposure is intended |
| `database` | SQLite path, including its associated WAL files |
| `interval_seconds` | Collector interval, from 10 to 86,400 seconds |
| `max_age_seconds` | Maximum original source age for a current rate |
| `allow_api_sources` | Explicit permission for configured API collectors |
| `rpc_url_env` | Environment variable for an optional RPC collector |
| `genesis_hash` | Expected Solana network identity |
| `sources` | Unique source configurations; can be empty in server-only mode |
| `concurrency` | Concurrent collectors, default 4 |
| `history_retention_days` | Retained history duration, default 30 days |
| `max_history_rows` | Retained history row limit, default 250,000 |

## Checks and container

From the repository root:

```sh
cargo test --locked -p yield-indexer
cargo clippy --locked -p yield-indexer --all-targets -- -D warnings
cargo build --locked -p yield-indexer
python3 scripts/smoke.py --binary target/debug/yield-indexer
docker build -t solana-yield-api .
```

The smoke check uses a synthetic local RPC server. It does not contact a live
provider. The container builds from the repository root and runs as user
`10001`. Mount a configuration at `/config/config.json` and writable storage
at `/data`. The default container mode is `--serve-only`. The image includes a server
configuration with `listen` set to `0.0.0.0:8080` and `database` set to
`/data/yields.sqlite`. Mount a private configuration to replace it when needed.

See [CONTRIBUTING.md](../../CONTRIBUTING.md) for contribution checks. This
crate uses the repository's [MIT license](../../LICENSE).
