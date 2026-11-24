# Native account observer

`scripts/live-yield-shadow.py` reads an existing local native account feed and
passes verified Bank batches to `yield-parser`. It does not create the account
feed or configure a provider. It makes no RPC requests and sends no
transactions.

## Source requirements

The feed must implement the pinned `blockzilla-native-account-bridge-v1`
contract over loopback HTTP:

- `POST /v1/watch`: a complete bootstrap snapshot, then ordered updates.
- `POST /v1/read-at`: the same account state at an exact Bank.
- `GET /health`: matching source identity, account-watch readiness, and zero
  RPC reads.

The operator supplies the expected source identity in a JSON pin file and a
provider token in a separate file. The observer requires the Solana mainnet
genesis. The source identity is an input verified by the operator; a digest does
not independently prove provider trust or canonical consensus.

The observer checks the original Bank observation age before and after parsing.
The maximum is 5,000 milliseconds. It also checks state digests, consecutive
cursors, exact Bank parent links, complete bootstrap keys, and zero RPC counters.
The parser requires Clock from the same Bank slot.

## Watch list

The public watch lists are:

- `crates/yield-parser/fixtures/live-save.json`: 11 Save reserves.
- `crates/yield-parser/fixtures/live-project0.json`: 8 Project 0 banks.
- `crates/yield-parser/fixtures/live-vaults.json`: 8 Elemental pools, 1 Hylo
  Earn Pool, and 1 Meteora Dynamic Vault.

Together they require 33 account keys, including Clock. The account observer
does not yet include Kamino or Jupiter account batches.

## Run

Build the parser from the repository root:

```sh
cargo build --locked --release -p yield-parser
```

The observer must run in a Linux network namespace where only the loopback
interface is present. An operator must supply that environment and make the
verified account feed available there. The following command assumes those
prerequisites already exist; it does not create them:

```sh
python3 scripts/live-yield-shadow.py \
  --base-url http://127.0.0.1:8081 \
  --token-file /path/to/private/token \
  --native-pin /path/to/private/source.json \
  --parser target/release/yield-parser \
  --markets crates/yield-parser/fixtures/live-save.json \
    crates/yield-parser/fixtures/live-project0.json \
    crates/yield-parser/fixtures/live-vaults.json \
  --duration 300 \
  --report /path/to/private/account-report.json
```

The script rejects a host with other network interfaces. It is not a command
for a normal developer laptop network. Runtime setup is provider-specific and
is intentionally outside the source repository.

## Report meaning

The report records accepted frames, exact-Bank read checks, maximum accepted
age, gaps, unavailable products, last values, and value-change counts. It is not
a complete share-price time series. It does not calculate trailing yield.

Save and Project 0 report stored share values. Elemental reports a posted APR.
Hylo reports a share value before withdrawal fees. Meteora calculates the
unlocked vault value using Clock. Each method has a different meaning; do not
merge them into one unlabeled APY.

The observer currently writes a local JSON report. It does not write directly
to the backend API's SQLite store. See the [roadmap](roadmap.md) for that
integration and [evidence record](evidence.md) for current test coverage.
