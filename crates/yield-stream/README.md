# Blockzilla yield stream

This service receives live Blockzilla bank-v2 payloads, validates the compact
block locally, selects transaction candidates, and decodes protocol instruction
bytes. It keeps candidates when logs are absent, truncated, or invalid. It has
no RPC or yield API configuration and sends no transactions.

The first parser is **Jupiter Lend liquidity**, program
`jupeiUmn818Jg1ekPURTpr4mFo29p46vygyykFJ3wZC`. This is useful because its native
instruction layouts expose operation requests and reserve identities. Six
methods are supported: `operate`, `pre_operate`, `update_exchange_price`,
`update_rate_data_v1`, `update_rate_data_v2`, and `update_token_config`.
Unknown or incompatible instructions keep their raw bytes and a decode status.
Optional event logs can add exchange prices and reported rates.

## Pipeline

```text
Blockzilla SDK -> checked compact block -> local program filter
  -> retained transaction + outer/CPI instruction parser
  -> optional log enrichment
  -> SQLite candidates/instructions + bank controls + cursor -> SDK acknowledgement
```

The filter checks static and loaded address-table keys. Every skipped transaction
still passes the shared decoder's validation. The main parser uses the exact
instruction program, resolved accounts, discriminator, arguments, and original
outer/CPI positions. A program key alone does not prove a call.

Failed transactions and unknown execution status remain stored. Their requests
cannot produce successful state changes. A successful transaction proves its
top-level calls completed; CPI metadata alone proves an attempted call because
a parent can catch an inner failure. CPI records remain `attempted_cpi_unproven`.
All decoded arguments are `requested_only`; sentinels are not transferred amounts.

The optional log parser checks the active runtime invocation. Missing, truncated,
or invalid logs mark enrichment as incomplete and never remove candidates or
instruction facts. The legacy `parser_rejections` counter counts log enrichment
failures; `log_enrichment_failures` is its explicit alias. The normalized retained
transaction keeps instruction, balance, loaded-address, and CPI metadata and
omits optional log text. Rejected or unsupported instructions keep their raw data.

Old captures keep their original facts. Adding the candidate tables does not
silently reparse earlier banks. Use a new capture database for complete coverage
under this parser version, or an explicit historical rebuild when available.

The parser retains instruction candidates when event logs are truncated.
The tests cover incomplete logs and recorded CPI instructions.

| Event | Saved fields | Yield limit |
| --- | --- | --- |
| `LogOperate` | User, token mint, signed supply/borrow amounts, destinations, supply/borrow exchange prices | Does not report the updated borrow rate |
| `LogUpdateExchangePrices` | Token mint, supply/borrow exchange prices, borrow rate, utilization | A rate at this event position; does not establish a complete current reserve state |

Wide values are decimal strings in JSON. Exchange prices use the protocol's
`10^12` scale. Borrow APR is `borrow_rate_bps / 10_000`; it is a borrowing cost.
The tool does not create supply APY or a current yield observation from these
events. Protocol amount units must be resolved before display as token amounts.

The layouts are pinned to the
[official Jupiter liquidity IDL](https://github.com/jup-ag/jupiter-lend/blob/33a22cf7a5bfdd32ab1712dda4adfbeb9b348ad9/target/idl/liquidity.json).
Parser source records the rate-unit reference and exact revisions.

## Run a local capture

This command is separate from the snapshot/API service. From the repository root:

```sh
cargo build --release --locked --manifest-path crates/yield-stream/Cargo.toml
```

### Replay a local binary archive

```sh
./target/release/yield-stream \
  --replay-archive /path/to/admitted/manifest.json \
  --report /path/to/yield-ledger-report.json
```

This mode reads the manifest and its local `<slot>.bin` files. It checks
the archive object lengths and digests, matches the decoded block identities,
and checks the parent chain before releasing the report. The operator supplies
the admitted manifest and verifies its origin. This command uses no RPC or
account snapshots. It records transaction-level evidence and gaps for
supported Jupiter and Kamino records. See the
[ledger parser contract](../yield-parser/README.md#read-measurements-from-the-ledger).

### Live capture

Copy `examples/config.json` to a private local configuration. Set the key file,
verified source file, and a new SQLite path. The source file must contain the
four verified `SourceIdentity` fields: `genesis_hash`, `node_identity`,
`agave_revision`, and `session_id`. The source identity is an input; the tool does
not replace it with the identity of an unverified server. The backend Bearer key
must be in a small regular file with mode `0600` or stricter. It is never placed
in a URL, stored in SQLite, or printed.

```sh
./target/release/yield-stream \
  --config /path/to/private/config.json \
  --duration-seconds 60 \
  --report /path/to/private/report.json
```

`CARGO_TARGET_DIR` can select a shared build directory. The default capture
duration is 60 seconds. Zero runs until a signal or storage capacity limit. The
tool handles SIGINT and SIGTERM and prints progress about every ten seconds.
A source change, expired replay cursor, invalid frame, or failed database commit
stops the run. It never silently skips a gap or selects a new head on restart.

First capture starts at the exact selected processed payload and replays all
subsequent notices, including children published before its confirmation. Older
history is excluded. Restart uses the saved cursor. Native confirmed and root
controls update each exact bank's event status; invalidation removes that bank
and its captured descendants from valid results. A slot alone is not a bank ID.
Processed rows are provisional. The status of a saved event is not proof that
the service is currently connected or that the event describes the latest rate.

## Parent-chain continuity

The intake cursor and verified rooted coverage are separate checkpoints. The
first checks ordered stream notices. The second checks every produced parent
back to the exact captured bank or the saved verified root. Each link requires
the parent slot, parent blockhash, and native height increment to agree. A root
does not skip this check when another root lies between it and the saved anchor.

A jump from slot 100 to slot 104 is valid when slot 104 names slot 100 and its
hash as parent, and native block height increases by one. Slot numbers alone do
not establish a missing produced block.

After reconnect, the tool resumes at its committed cursor. It restores and
checks the saved verified path before accepting further coverage. Missing,
ambiguous, invalidated, or inconsistent new ancestry sets `continuity.status` to
`gap` and keeps `verified_root` unchanged. A missing parent report includes the
expected slot and hash. Late parent evidence can repair the path. The intake
can retain provisional candidates while a gap remains; it must not claim
complete historical coverage for them.

Restart recomputes report fields and checks every covered-bank marker against
the full saved path. Contradictory evidence that breaks an already verified
prefix stops intake before commit. A late root on a different captured branch
sets a gap even when its height is below the saved root.

The continuity report, covered-bank markers, parsed facts, and intake cursor
commit together before SDK acknowledgement. Native bank finality and complete
parsed-history coverage remain separate. Parent hashes do not prove complete
CPI data, full account writes, or independent consensus.

Check an existing local capture without a network connection or key read:

```sh
./target/release/yield-stream \
  --config /path/to/private/config.json --check-continuity \
  --report /path/to/private/continuity-report.json
```

This command adds continuity metadata to an older database from its saved native
bank and control evidence. It does not move the intake cursor or add missing
parser facts. Later checks validate the saved root path before returning it.
The integration tests in `tests/continuity.rs` check these conditions.

Bank/control metadata, selected normalized transactions, instruction facts, and
optional events are stored. The tool does not retain all raw block payloads.
It stops before 10,000 bank records, 50,000 frame/control records, or 100,000 rows
in each candidate, instruction, or event table. These limits make the local test
bounded. Production pruning and archive recovery remain later work.

Set `sample_frame` in the configuration to a new file path to save at most one
block with candidates. On restart it checks and reuses that sample. The tool
refuses to overwrite an invalid existing sample. Replay it
without a network connection or key read:

```sh
./target/release/yield-stream \
  --config /path/to/private/config.json \
  --replay-frame /path/to/private/sample.frame
```

Replay checks the frame digest, pinned source, bank identity, compact data, and
parser output. The raw frame has processed status; later confirmations belong
to the live database. This command does not claim historical backfill coverage.

To check one retained block again through the SDK, use `--fetch-frame SHA256`
with the same configuration. This command checks the pinned source and frame
digest and does not advance the capture cursor. It needs the backend key and
stops if that block is no longer retained. It is a diagnostic read, not a
complete backfill process.

Read instruction records from finalized banks as JSON without a network connection:

```sh
python3 scripts/read-stream.py /path/to/private/stream.sqlite --limit 10
```

Use `--status all` to inspect provisional or invalidated rows. The status is
always included in each result. `history_contiguous` reports whether the bank
belongs to the verified rooted path after capture. It does not prove complete
parser coverage under a later parser version. `--kind candidates` returns retained normalized
transactions. `--kind events` returns optional log events. A finalized bank can
contain a failed transaction; check `execution_status` separately.

For historical source selection and coverage, see [the backfill design](BACKFILL.md).

## Checks and source export

```sh
cargo test --locked --manifest-path crates/yield-stream/Cargo.toml
cargo clippy --locked --manifest-path crates/yield-stream/Cargo.toml --all-targets -- -D warnings
```

The stream tool is a member of the root Cargo workspace. It uses the public
parser library, the compact decoder, and the pinned Blockzilla SDK under
`vendor/blockzilla`. Build and test it from the repository root.

## Recorded evidence

See [the evidence summary](../../docs/evidence.md). Earlier tests establish
bounded parser and stream behavior. They do not establish current source
availability, complete account state, or all-protocol yield coverage.
