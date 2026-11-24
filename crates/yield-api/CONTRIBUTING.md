# Contribute to yield-indexer

Keep the service independent. Use explicit Cargo dependencies and configuration.
Do not import a parent product service, private path, deployment setting, or
credential. New adapters belong under `src/sources/onchain` or
`src/sources/api`, according to the data authority.

## Adapter contract

Each adapter must define:

- A stable source ID, observation ID, protocol, market, asset, and denomination.
- A named calculation method and explicit annual fraction units.
- A separate rate component and `apr` or `apy` kind for each result.
- Original source time, collection time, and an explicit unavailable reason.
- Evidence that identifies the data origin and transport.

Return the shared `Observation` type and satisfy `Observation::validate`.
Components are never added or compounded by implication. Document whether a
provider total already includes rewards. Keep incentive estimates separate from
native interest. Never turn missing data into a zero rate.

Chain adapters must check the expected network, account owner, discriminator,
layout, decimals, and account relationships. Use finalized context and the chain
Clock as observation evidence. Read all dependent accounts with a consistent
slot rule. Do not label RPC collection time as chain time. A public account
address or successful response alone does not establish a valid market.

API adapters require the explicit `allow_api_sources` option. They retain
`origin: "api"`, even when the provider describes an on-chain source. Set
timeouts, body limits, and row limits. Validate the provider's source timestamp.
A catalog generation time or HTTP fetch time cannot renew an old observation.
An explicit unavailable latest rate cannot fall back to an older value.

Preserve observation history when a later read fails. Current availability must
show missing, malformed, stale, or failed data. Add coverage only for adapters,
markets, and fields that the implementation and tests support. Counts in an
external catalog do not establish native protocol coverage.

Do not write private URLs, credentials, full provider errors, or private account
state into logs or fixtures. Use public fixtures with a source and revision.
Document the license and source of copied protocol layouts or calculations.

## Required checks

From the repository root:

```sh
cargo fmt --package yield-indexer -- --check
cargo test --locked -p yield-indexer
cargo clippy --locked -p yield-indexer --all-targets -- -D warnings
```

The repository workspace owns `Cargo.lock`. Keep protocol parser changes in
`crates/yield-parser` and HTTP or storage changes in this crate. Update
[the API guide](../../docs/api.md), [OpenAPI](../../docs/openapi.json), and
examples together when the service contract changes.

Tests must use saved public fixtures or local mock servers. Add tests for the
rate formula, scale and units, account identity and layout checks, invalid data,
original time, unavailable latest values, and stale observations. Test API body
and row bounds without network access. A unit test does not prove current live
market availability or complete historical coverage.

For an intentional live check, select the source and its public account set,
keep credentials outside the repository, and save only a bounded result with
its revision and evidence. Do not make live provider access part of the normal
test suite.

## Pull requests

State the source and method added, the supported markets, the evidence retained,
and the exact test results. Explain any missing rate component or historical
limit. Include configuration documentation and a public fixture where possible.
Update API schemas and examples together when their contract changes.

Contributions to this service are under [the MIT license](LICENSE). Publishing a package or container is a separate release step.
