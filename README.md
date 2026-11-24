# Solana Yield

Open-source Solana yield parsers, a Blockzilla ledger reader, and a backend API.
The project stores source evidence with each observation and keeps three types
of data separate: ledger events, account values, and calculated returns.

This is an initial release. Seven protocols have scoped native parsers. The
[coverage registry](config/protocols.json) lists 46 protocol labels and the work
still required for each one. Current live yield coverage is not yet proved.

## What is included

| Component | Purpose |
| --- | --- |
| [Yield parser](crates/yield-parser/README.md) | Offline account and event parsers for Kamino, Jupiter, Save, Project 0, Elemental, Hylo, and Meteora Dynamic Vault |
| [Blockzilla stream](crates/yield-stream/README.md) | Read and validate Compact V2 blocks, retain Jupiter instruction facts, and replay supported ledger measurements |
| [Backend API](docs/api.md) | SQLite observation history, current availability, source status, protocol coverage, and protocol filters |
| [Account observer](docs/account-feed.md) | Read verified account-feed frames with a five-second Bank observation limit and zero RPC |
| [Protocol registry](docs/coverage.md) | Implemented layouts, test evidence, missing inputs, and candidate protocols |

New account parsers use a [shared adapter contract](docs/adding-a-protocol.md).
It provides named account requirements, a checked Bank context, and common
measurement checks. A runnable external-adapter example is included.

The parser performs no network requests. The Blockzilla stream and account
observer send no transactions and make no RPC requests. Backend RPC and
external API collectors are separate, explicit options. The default quickstart
starts only the HTTP API.

## Quickstart

Use Rust 1.99 or later. Python 3.11 or later is required only for the helper
scripts. Run these commands from the repository root:

```sh
cargo test --workspace --locked
cargo run --locked -p yield-parser --quiet -- \
  < crates/yield-parser/fixtures/kamino-reserve-usdg.json
cargo run --locked -p yield-indexer -- \
  --config crates/yield-api/examples/config.server.json --serve-only
```

In another terminal:

```sh
curl --fail http://127.0.0.1:8080/health
curl --fail http://127.0.0.1:8080/v1/protocols
curl --fail 'http://127.0.0.1:8080/v1/yields?protocol=Jupiter&limit=20'
curl --fail http://127.0.0.1:8080/openapi.json
```

A new database has no observations, so the yield list is empty and health
reports `ready: false`. `--serve-only` opens the API without starting a collector
or making outbound requests. See [API setup](docs/api.md) for the separately
configured collectors and the exact response contract.

The native stream, account observer, and API store are separate components in
this release. A single native ingestion path into the API store is part of the
[roadmap](docs/roadmap.md).

## Repository layout

```text
crates/
  yield-parser/           Offline library and command-line parser
    src/protocols/        One directory for each implemented protocol
  yield-api/              HTTP API, SQLite storage, explicit source collectors
  yield-stream/           Compact V2 capture and archive replay
  blockzilla-decoder/     Shared block decoder
config/
  protocols.json          Protocol coverage and missing inputs
docs/                     API, source contracts, evidence, and roadmap
scripts/                  Account observer and local checks
vendor/blockzilla/        Pinned, licensed source dependency snapshot
```

## Evidence and limits

- **Ledger event:** a program emitted a value at a transaction position. The
  parser checks execution and available invocation evidence. Missing logs can
  leave measurement gaps even when every block is present.
- **Account value:** verified bytes from one Bank contain a stored share value
  or posted rate. A fresh Bank does not make an old protocol update new.
- **Calculated return:** a result from comparable observations over a stated
  interval. It needs the same asset, market, method, and source basis.

A borrow APR is a cost. A posted APR is not a realized return. A stored share
value is not an APY. No component silently combines these measurements.

The saved historical Compact V2 test found 294 Jupiter operation-price
observations in 128 blocks with zero RPC calls. The 29-product account-feed
check used synthetic frames; its live preflight did not pass. No current
all-protocol live yield claim follows from those tests. See
[the evidence record](docs/evidence.md) and [protocol coverage](docs/coverage.md).

## Development

```sh
cargo fmt --package yield-parser --package yield-indexer --package yield-stream --package blockzilla-decoder -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked --no-deps -- -D warnings
```

Read [CONTRIBUTING.md](CONTRIBUTING.md) to add a parser, source adapter, or test.
The [withdrawal-check design](docs/redemption-validation.md) explains the planned
optional execution checks and keeps them separate from measured share values.
Read [SECURITY.md](SECURITY.md) before reporting a possible vulnerability.
The code uses the [MIT license](LICENSE). Included dependencies retain their
own license files and source records.
