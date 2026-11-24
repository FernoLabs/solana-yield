# Blockzilla native schema source snapshot

This directory contains the exact source dependency closure for the native
`native-compact-projection-v1` live block decoder, the strict
`native-bank-compact-v2` contract and the shared Rust bank client. Each package has explicit
metadata and lints. No package inherits settings from the Ferno workspace.
Use relative path dependencies on its schema crates. It requires Rust 1.98 or later.
The snapshot is outside the indexer package directory. The Ferno workspace
excludes `vendor/blockzilla`, so its standalone test targets and default native
features are not selected by Ferno workspace checks.

The base Blockzilla revision is recorded in `snapshot.json`. The source working
copy had local changes. The commit alone does not identify this client or decoder. The manifest records
the source hash and copied hash of every file.
Copied crate manifests set package metadata and lints explicitly and omit
standalone benchmark and development dependencies.
The original MIT license is in `LICENSE`.

One source adaptation permits a portable native build. The registry crate's
`native-registry` feature selects the original native MPHF and gxhash path and
is enabled by default. With that feature off, the existing upstream wasm
`HashMap` code is also used on native hosts. The indexer selects
`default-features = false` and `portable-registry` on the compact and archive
schema crates. The live decoder uses the exact one-based byte dictionary and
does not read an MPHF index. This change does not change any wire schema, key
ID, or decoder result. It removes the unused AES CPU requirement from generic
ARM and x86 Linux builds. The manifest records both versions of each adapted
file. The snapshot script applies the adaptation again on refresh.

The Ferno adapter uses the frame checks and native semantic decoder in these
Blockzilla source files. Their exact source hashes are in the same manifest:

- `hivezilla/stream-relay/src/frame.rs`
- `hivezilla/stream-relay/decoder/src/lib.rs`
- `hivezilla/stream-relay/examples/rust-consumer/src/main.rs`
- `crates/compact-v2/blockzilla-compact-v2-reader/src/signed_message.rs`

The transport uses the actual SSE notice and finite chunk GET routes. The
adapter resolves dictionary keys, compact metadata and compact logs. It keeps
the source marker's operator-trusted finality and source identity. It does not
claim independent proof of a canonical chain.

The shared bank client has no native semantic decoder dependency. The yield
stream passes admitted payloads to the portable compact decoder. See
[the stream guide](../../crates/yield-stream/README.md) for the consumer contract.

Verify this snapshot without access to the source repository:

```sh
python3 vendor/blockzilla/snapshot.py --verify
```

The recent finalized binary adapter has a separate targeted update in
`snapshot.json`. It adds the exact upstream `ssh_beam.rs`, its protocol tests,
and the `finalized::BlockId` module. The earlier HTTP client and v2 wire code
stay pinned; their only source changes expose those modules. Tokio process,
I/O and macro features and the client protocol test dependencies are enabled.
The adapter reads original native frames through a finite, source-bound SSH
request. This is rolling retained coverage, with no complete epoch claim.

To refresh from an explicitly selected source working copy, run the same script
with the source repository path. Review the changed hashes, schema, and adapter
tests before use. Do not replace this snapshot with a sibling absolute path.
