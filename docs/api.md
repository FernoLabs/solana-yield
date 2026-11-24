# HTTP API

The API serves stored yield observations over HTTP. It has no write routes.
It binds to a local interface by default. The machine-readable contract is
[openapi.json](openapi.json); the service also serves it at `/openapi.json`.

## Start without network collectors

From the repository root:

```sh
cargo run --locked -p yield-indexer -- \
  --config crates/yield-api/examples/config.server.json --serve-only
```

This mode opens the configured SQLite database and serves its saved data. It
starts no collectors and creates no RPC client. A new database is empty and
reports `ready: false`. The example uses a five-second maximum source age.
The server rechecks that age on each current-data request.

Collection needs explicit source configuration. See the
[service guide](../crates/yield-api/README.md). Blockzilla account and ledger
capture currently use separate stores; importing their values into the API is
planned work.

## Routes

| GET route | Result |
| --- | --- |
| `/health` | Process/database status and current source readiness |
| `/v1/protocols` | Bundled protocol parser coverage and planned work |
| `/v1/sources` | Stored source configuration status |
| `/v1/yields` | Latest observations, with filters and pagination |
| `/v1/history?source_id=...&id=...` | Stored history for one source and position |
| `/v1/coverage` | Coverage and availability of data currently in the database |
| `/openapi.json` | OpenAPI 3.1 specification |

`/v1/protocols` describes implementation and recorded verification. It does not
indicate that a live feed is connected. `/v1/coverage` describes collected data;
its counts do not include every known or planned market.

## Filter and page observations

```sh
curl --fail 'http://127.0.0.1:8080/v1/yields?protocol=Kamino&origin=onchain&offset=0&limit=20'
curl --fail 'http://127.0.0.1:8080/v1/history?source_id=kamino-usdg&id=kamino-usdg&limit=20'
```

`protocol` matches any name in an observation's protocol lineage. Matching is
case-insensitive. Reviewed Kamino and Jupiter aliases are normalized. A vault
that lists Kamino as an underlying protocol can therefore match `Kamino` even
if its primary protocol differs. Unknown protocols retain their own names.

`origin` is `onchain` or `api`. Filters run before pagination. Rows have a
stable order by `source_id` and `id`. `total` is the number of rows after all
filters. The current endpoint defaults to 200 rows and permits 1 to 1,000.
`offset` permits 0 to 1,000,000. History defaults to 100 rows, permits 1 to
1,000, and returns the newest stored observations first.

An unknown query field, invalid value, or missing required field returns HTTP
400. Query decoding errors can be plain text; explicit validation errors have
`{"error":"invalid_query"}`. Database failures return HTTP 503 with
`{"error":"database_unavailable"}`.

## Read a value correctly

Each observation keeps `source_id`, `id`, `protocol`, `protocols`, `method`,
`denomination`, original `observed_at_ms`, `collected_at_ms`, and `evidence`.
Rates have a component, `apr` or `apy` type, and an annual fraction. For example,
`0.05` means 5%. A borrow APR is a cost. Components are never added implicitly.

Missing, failed, or stale data stays visible with an empty `rates` array and
an `unavailable_reason`. A new HTTP response or process restart does not renew
the original source time. Historical values retain their original facts and
can be older than the current freshness limit.

HTTP 200 from `/health` means the process and database can answer. Check `ready`
and each row's availability before using current rates. A fresh Bank observation
is also different from a protocol's older stored refresh time. The separate
account parser retains both when the source supplies them.
