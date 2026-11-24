# Contributing

Start with [protocol coverage](docs/coverage.md) and the
[roadmap](docs/roadmap.md). A protocol name does not identify one account
layout. Scope a change by program, account type, version, and calculation.

## Add a protocol parser

Use [the shared adapter guide](docs/adding-a-protocol.md) and its runnable
example. Implement `ProtocolAdapter` and call the common `adapter::observe`
runner. The library supports external adapters without a central enum change.


1. Find the protocol's official source or IDL. Record its exact revision and
   the account layout or event discriminator used by the parser.
2. Add a module under `crates/yield-parser/src/protocols/<protocol>/`.
   Keep byte parsing and calculation independent of network and storage code.
3. Validate the owner, discriminator, length, initialization, mint, market,
   and relevant account relationships. Reject unsupported layouts explicitly.
4. Use exact integer ratios for stored share values. State the units, scale,
   fee treatment, and update fields. Do not label a ratio as an APY.
5. Add tests for valid data and for the identity, layout, arithmetic, and time
   failures that could change a value. State whether a fixture is synthetic
   or contains public historical account bytes.
6. Register the command-line JSON configuration only if needed. Add the module
   to the public parser API and update
   `config/protocols.json` and `docs/coverage.md`.

A live account adapter must bind the bytes, Clock, and observation time to one
verified Bank. Keep the protocol's last update time. Do not replace that time
with the time at which a client fetched the record.

## Add a source or API change

Keep source identity, transport, commitment, observation time, and collection
time available in the stored result. A source error or stale value must make
the current result unavailable. Preserve prior observations as history.

Native observations, external API values, and calculated returns need separate
methods. An external source cannot claim native account evidence. A parser
status in the protocol registry does not prove runtime service readiness.

Update `docs/api.md` and `docs/openapi.json` when the HTTP contract changes.
Test filters together with pagination. Do not return unfiltered totals as
filtered coverage.

## Checks

From the repository root:

```sh
cargo fmt --package yield-parser --package yield-indexer --package yield-stream --package blockzilla-decoder -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked --no-deps -- -D warnings
```

Use focused package tests while developing. The offline parser can be tested
with `cargo test -p yield-parser --locked`. The default test suite must not
require credentials, a live provider, or a transaction signature.

For a live test, state its source, network, time window, commitment, observation
age limit, and expected measurements. Keep credentials and raw runtime files
outside version control. Do not change a source or send a transaction as part
of a read-only parser test.

## Pull requests

Describe the input, the resulting behavior, and the limits. Include the source
revision and the tests that ran. Explain any wire or output-format change.
Do not describe a synthetic test as live evidence or a historical capture as
proof of current availability.

Do not commit credentials, private account records, local provider settings,
database files, or build output. Small public fixtures need a source note and
a clear purpose. Keep third-party license files and pinned source records.
