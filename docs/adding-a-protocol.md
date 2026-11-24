# Add an account parser

Use the shared account-adapter contract in
[`adapter.rs`](../crates/yield-parser/src/adapter.rs). An adapter supplies:

1. A stable protocol identifier.
2. Named account requirements for one configured market.
3. A pure `read` method that returns exact measurements.

The shared runner obtains declared accounts, checks their basic identity and
initialization, and validates the output. Protocol code checks the exact owner,
layout version, discriminator, asset and market relationships, and arithmetic.
Transport, source admission, storage, and return calculations stay outside it.

## Example

Run the self-contained example without a network or a database:

```sh
cargo run --locked -p yield-parser --example custom_adapter
```

[`custom_adapter.rs`](../crates/yield-parser/examples/custom_adapter.rs) defines
an adapter outside the library. It uses synthetic account data and does not
represent a real protocol. The example shows the three trait methods, an
immutable account provider, the same-Bank Clock check, and exact output units.
No central library enum must change for a caller to use its own adapter.

For built-in protocols, put the adapter in
`crates/yield-parser/src/protocols/<protocol>/adapter.rs`. Keep the decoder and
protocol arithmetic in that protocol's module. Existing direct functions stay
usable when a caller only needs one account layout.

## Shared checks

- `AccountSet` accepts at most 100 unique account records.
- `ObservationContext` checks Clock against the supplied nonzero Bank slot.
- `adapter::observe` validates protocol/role identifiers and required accounts.
  The read method can access only its declared accounts and Clock.
- Account addresses must match the request. A read account needs a valid owner
  key, positive lamports, and a non-executable account envelope.
- Results need a nonzero denominator and a unique measurement kind. Zero
  numerators remain valid when the protocol permits them.
- Stored update slots and times cannot be later than the Bank context.
- Missing inputs, unsupported layouts, and invalid results return an error.

The caller must admit the source and prove that all bytes belong to the same
Bank. A matching Clock does not authenticate other accounts. The provider must
remain immutable during the entire observation. The live collector applies the
five-second age limit; historical readers retain the original time instead.

## Register and test

1. Pin the official program source or IDL and record the layout version.
2. Implement the adapter and its protocol-specific validation.
3. Test public or synthetic fixtures, wrong owners and mints, unsupported
   layouts, arithmetic boundaries, and missing accounts.
4. Use the shared runner in the tests. Include a zero value if it is valid.
5. Export the module from `protocols/mod.rs`. Register JSON configuration in
   `live_batch.rs` only if the command-line batch interface needs it.
6. Update `config/protocols.json`, the coverage guide, and the source notes.

The account observer uses the parser's `watch_list` mode to discover required
read keys, including Clock. It does not need a second list of protocol account
roles. Watch-key discovery does not verify source provenance or account data.

The library interface is independent of JSON configuration. A new account
adapter does not need to change the API service, ledger transport, or account
provider. A new ledger event still needs its program, event layout, and output
type registered in the existing shared ledger parser.

```sh
cargo test --locked -p yield-parser
cargo test --locked -p yield-parser --no-default-features
cargo clippy --locked -p yield-parser --all-targets -- -D warnings
```

A withdrawal simulation is a separate optional check. See
[withdrawal validation](redemption-validation.md); it must not add network or
transaction execution to the account parser.
