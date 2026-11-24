# Blockzilla compact decoder

This crate decodes native Compact V2 ledger bytes. It has no HTTP client, RPC
client, or database. The caller supplies admitted bytes and source evidence.
The schema dependency snapshot is pinned in
[the vendor manifest](../../vendor/blockzilla/snapshot.json).

`decode_bank_payload` returns transaction results and conservative changed
keys. `decode_bank_block` returns a Solana transaction projection.
`decode_bank_financial_block` selects a bounded interest set after all admission
checks. `DecodedBlock.transaction_indices` retains original transaction
positions; `total_transactions` counts all validated transactions.

A processed block does not prove confirmation or finality. The caller must
apply matching native Bank controls. This crate does not reconstruct account
state or supply missing account values.

`archive::decode_bundle` reads a local `BlockzillaGetBlockBundleV1` binary
response. It checks the requested slot, blockhash, parent constraints, and
height. The archive source, genesis, and coverage are supplied and verified by
the caller. It does not accept an RPC JSON block.

```sh
cargo test --locked -p blockzilla-decoder
python3 vendor/blockzilla/snapshot.py --verify
```

The `test-fixtures` feature exposes synthetic builders for dependent tests. It
is not a production data source. Preserve this crate's MIT license and the
licenses under `vendor/blockzilla` when copying the source.
