# Historical replay

The stream tool can read an admitted local Compact V2 binary archive. A live
capture and historical replay use the same protocol parser. Use an explicit
source manifest with an anchor, block hashes, object lengths, SHA-256 digests,
and a continuous produced-block parent path.

```sh
cargo run --locked -p yield-stream -- \
  --replay-archive /path/to/admitted/manifest.json \
  --report /path/to/report.json
```

The operator must verify the origin of the manifest before use. A correct
object digest verifies the supplied bytes, not the truth of the source claim.
The archive reader checks object integrity, decoded identities, and parent
continuity. It uses no RPC and makes no network request.

## Coverage

Keep produced blocks, missing blocks, missing execution metadata, missing CPI
records, and missing protocol measurements separate. A complete block range
can still contain no share-value checkpoint for a selected market. Slot gaps
alone do not prove a missing produced block: use parent hashes and heights.

Retain the source identity, blockhash, slot, block time, transaction position,
signature, and instruction position. Requests, successful operations, and
explicit exchange-price observations are different facts. Reprocessing with a
new parser version must be explicit; never silently update old facts.

The current tool does not fetch a remote archive, reconstruct all account
writes, or repair a provider's history. A future archive adapter must verify
admission and commit its output and cursor together. It must stop on missing
or inconsistent evidence instead of moving to a newer head.
