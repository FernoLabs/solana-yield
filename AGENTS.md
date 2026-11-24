# Working on Solana Yield

Use simple technical English. Check the branch, commit, and local changes
before editing. Keep existing local changes.

Read README.md, docs/coverage.md, and the guide for the changed crate. The
root Cargo.toml defines the workspace. Each implemented protocol has a folder
under crates/yield-parser/src/protocols/.

Keep ledger events, observed account values, and calculated returns separate.
Preserve the original source time, Bank identity, exact units, and method.
Missing, stale, unbound, or unsupported data must remain unavailable. Never
claim a posted rate or borrow cost is realized supply yield.

The native Blockzilla path is read-only and uses zero RPC. Its account
observer uses a five-second Bank observation limit. RPC and external API
collectors are explicit separate modes. Do not change another service or run
a live capture as part of an ordinary test.

Run the relevant crate tests and the repository checks before publishing:

```sh
cargo test --workspace --locked
cargo fmt --package yield-parser --package yield-indexer --package yield-stream --package blockzilla-decoder -- --check
cargo clippy --workspace --all-targets --locked --no-deps -- -D warnings
python3 scripts/check-repository.py
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 vendor/blockzilla/snapshot.py --verify
```

Keep credentials, provider source pins, captured databases, and local runtime
configuration outside tracked files. Preserve vendor source bytes and update
the source manifest explicitly when changing a dependency snapshot.
