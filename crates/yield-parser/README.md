# Yield parser

An offline Rust library and command-line tool for Solana lending account and
ledger data. The caller supplies verified account or transaction records.
The parser has no network, storage, or product dependencies.

| Input kind | Supported account | Output |
| --- | --- | --- |
| `kamino_reserve` | Kamino Lend Reserve v1 | Stored net reserve assets per collateral share. Accrued fees included in the account reduce the assets. |
| `jupiter_earn` | Jupiter Earn main program Lending account | Stored asset units per jlToken share. The stored conversion can include rewards. |

The values are exact fractions in raw asset units per raw share unit. A
collector can store two or more samples of the same account and calculate the
return per share over the observed period. The collector must verify the bank
and source time for each sample. Stored account state can precede the bank
that supplied it. Keep the stored share value, time-adjusted quotes, borrow
APR, and separate incentives as distinct measurements.
[Kamino program](https://github.com/Kamino-Finance/klend)
· [Jupiter Lend architecture](https://github.com/jup-ag/docs/blob/main/lend/architecture.mdx)

## Protocol folders

Each supported protocol has one folder under [`src/protocols`](src/protocols):

| Folder | Account reader | Ledger reader |
| --- | --- | --- |
| [`kamino`](src/protocols/kamino) | Stored reserve share value | Reserve exchange-rate getter |
| [`jupiter`](src/protocols/jupiter) | Stored Earn share value | Earn and Liquidity events |
| [`save`](src/protocols/save) | Reserve share value | Not implemented |
| [`project0`](src/protocols/project0) | Native bank share value | Not implemented |
| [`elemental`](src/protocols/elemental) | Posted deposit APR | Not implemented |
| [`hylo`](src/protocols/hylo) | Gross Earn Pool share value | Not implemented |
| [`meteora`](src/protocols/meteora) | Dynamic Vault share value | Not implemented |

The common Bank, Clock, and transaction checks stay outside these folders.
New library code can use `protocols::kamino::share_value`,
`protocols::jupiter::ledger`, or `protocols::save::observe`. The original
`kamino`, `jupiter`, `jupiter_ledger`, `kamino_ledger`, `live_save`,
`live_project0`, and `live_vaults` imports remain available.

## Run locally

From this directory:

```sh
cargo test --locked -p yield-parser
cargo run --locked --quiet -p yield-parser < fixtures/kamino-reserve-usdg.json
```

The CLI accepts one JSON object on standard input, up to 2 MiB. For an account
read, its
`account` object contains `address`, `owner`, `lamports`, `executable`,
and `data_base64`. Its `parser` object contains `kind`, `asset_mint`, and
`share_mint`. Use `lending_market` for Kamino or `liquidity_reserve` for
Jupiter. Set those expected keys from your adapter configuration.

The output contains the method, account and mint addresses, the program's
stored update fields, and `raw_assets_per_raw_share`. The numerator,
denominator, and update fields use decimal strings. Missing update fields
are null. The update fields describe the program's last refresh. Record
the separately verified snapshot Bank with each result. If the account
fails validation, the command returns a nonzero exit status and sends the
error to standard error.

## Read measurements from the ledger

Pass a resolved transaction as `{"ledger_transaction": <transaction>}`.
Use the compiled message form with `accountKeys`, `programIdIndex`,
`accounts`, and base58 instruction data. The transaction metadata supplies
the execution result, logs, address table keys, inner instructions, and
return data when recorded. The caller verifies the block source, transaction
position, and finality.

| Record | Measurement |
| --- | --- |
| Jupiter Earn `LogUpdateRates` | Stored share and liquidity exchange prices in units of `10^12`. The event omits the pool and mint. The parser binds a market for one verified top-level Earn invocation with a supported instruction. An unbound event has no market identity. |
| Jupiter Liquidity `LogUpdateExchangePrices` | Token mint, stored exchange prices, borrow APR in basis points, and utilization in basis points. |
| Jupiter Liquidity `LogOperate` | Token mint and the emitted exchange prices. The operation event supplies no updated borrow APR. |
| Kamino `calculate_ctoken_exchange_rate` return | The final top-level getter's reserve key and its exact returned Q60 value. Divide by `2^60 × 10^6` for raw asset units per raw collateral share unit. |

Log measurements require a successful transaction and a complete matching
invocation trace. Failed calls discard their measurements, including those
of their children. Missing, truncated, or invalid logs produce a status.
Kamino getter returns use independent return data and inner instruction
evidence. Use matched values as observations at the recorded transaction.
A block range can have gaps in measurements even when every block is present.
[Jupiter Earn IDL](https://github.com/jup-ag/jupiter-lend/blob/7960a14a5a760abc41db3ed1395dc5cb56a1f8db/target/idl/lending.json)
· [Jupiter Liquidity IDL](https://github.com/jup-ag/jupiter-lend/blob/7960a14a5a760abc41db3ed1395dc5cb56a1f8db/target/idl/liquidity.json)
· [Kamino getter](https://github.com/Kamino-Finance/klend/blob/686913ee421e0c5f3dc44c4c8344461ee0e4fb89/programs/klend/src/handlers/handler_calculate_ctoken_exchange_rate.rs)

## Use as a library

Build with `--no-default-features` to use the library without the CLI
dependencies. Call `kamino::share_value` or `jupiter::share_value` with an
`Account` from a verified snapshot and the expected public keys. Enable the
`ledger` feature to parse a transaction with `ledger::parse_transaction`.

## Parse live account batches

The CLI also accepts `{"live_bank":{"slot":<slot>,"entries":<accounts>,"markets":<markets>}}`.
Each entry is keyed by its Solana address and contains the native account-feed
record or an explicit null. A batch needs the Clock from the same Bank. The
caller checks the source, Bank identity, account digest, and observation age
before it passes the batch to the parser.

| Venue | Products in the watch list | Measurement |
| --- | ---: | --- |
| Save | 11 | Stored assets per raw collateral share, net of reserve fees |
| Project 0 | 8 | Stored asset share value for a native lending bank |
| Elemental | 8 | Posted simple annual rate |
| Hylo | 1 | hyUSD per eHYUSD share before withdrawal fees |
| Meteora Dynamic Vault | 1 | Unlocked USDT per LP share at the Bank's Clock |

The batch reports exact ratios and per-product unavailability. A stored
reserve refresh time or slot stays separate from the Bank observation time.
Keep the protocol's last update time even when a fresh Bank returns the
same stored value. Calculate trailing returns from comparable observations
over a documented period.

Public watch lists are in [Save](fixtures/live-save.json),
[Project 0](fixtures/live-project0.json), and
[Elemental, Hylo, and Meteora](fixtures/live-vaults.json).

One venue can use several layouts. Many markets can use the same layout. The
[coverage map](COVERAGE.md) lists the next parser and data source work by
protocol family. [License](LICENSE)
