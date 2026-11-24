# Evidence record

This record separates saved historical checks, synthetic tests, and current
live acceptance. It is not a statement that a provider is connected now.

## Parser checks

Before extraction, the parser passed 39 local Rust tests. A check with the
current parser reproduced all 12 expected measurements from five selected
transactions in the saved historical sample. These results check the parser
and its inputs; they do not establish current source freshness.

The repository includes account and synthetic test fixtures. Source notes in
`crates/yield-parser/fixtures/` identify the public historical account bytes.
A placeholder envelope field in a fixture is not original on-chain evidence.

## Historical Compact V2 archive

The saved archive covers 17 November 2026, 21:07:02 to 21:07:28 UTC. Its report
records:

| Field | Recorded result |
| --- | ---: |
| Blocks | 128 |
| Slot range | 470678385 to 470678512 |
| Transactions | 50,540 |
| Candidate transactions | 314 |
| Successful / failed candidates | 147 / 167 |
| Jupiter Liquidity operation-price observations | 294 |
| USDC / USDT / SOL observations | 116 / 152 / 26 |
| Parser errors / reported gaps | 0 / 0 |
| RPC calls / network calls during replay | 0 / 0 |

The report verified the supplied parent chain. It explicitly did not claim
complete canonical history. It found no Jupiter Earn event, Kamino getter
observation, or borrow-rate update event. The large capture and its private
runtime records are not included in this source repository.

An earlier 16 October 2026 live capture recorded 80 Jupiter Liquidity events
in its first 120 seconds: 78 operations and two exchange-price/rate events.
Those dated results are evidence for that window only.

## Account-feed tests

The combined test covered 29 configured products and 33 account keys, including
Clock: Save 11, Project 0 8, Elemental 8, Hylo 1, and Meteora 1.

The 65-second synthetic test accepted 164 frames and 4,756 values. It rejected
a frame aged 5,001 milliseconds, an invalid state digest, and a frame that
reported nonzero RPC use. These were synthetic frames, not live yield data.

The saved live preflight failed because the native head was unavailable.
No successful current live account run for all 29 products is recorded here.

## Next acceptance test

Use the selected Blockzilla feed without changing another service. First check
that its verified native account head is available. Then run a bounded read-only
capture and record:

1. Pinned network and source identity, exact Bank identities, and parent links.
2. Original Bank observation age of at most five seconds at acceptance.
3. Exact-Bank account comparisons, matching Clock, and state digests.
4. Zero RPC reads and no transaction submission.
5. Per-product values, missing values, update times, and watch gaps.

Keep ledger events, account values, and calculated returns separate in the
report. A quiet market or an unchanged stored value does not alone prove zero
return. A return needs a stated interval and comparable endpoints.

## Standalone repository checks

The extracted workspace passed 215 Rust tests on 24 November 2026. One test
that requires an operator-supplied live frame stayed disabled. All five offline
account-feed admission tests passed, including rejection beyond five seconds.
The source check covers local documentation links, JSON, and known credential
patterns. The vendor check verified 119 files against their recorded hashes.
These checks do not establish current live yield coverage.

The final standalone check also passed Clippy with warnings denied for the
workspace code, both CLI/API startup checks, and a replay of all 12 saved
measurements through the reorganized parser. The RPC-adapter startup check
used a synthetic loopback server; it was not a live RPC call. No live provider
capture or transaction submission was performed. A Docker image build was
not run because the local Docker daemon was unavailable.

## Shared account-adapter checks

The common account-adapter change passed 229 workspace Rust tests and seven
Python checks. The parser accounts for 53 Rust tests, including an external
adapter contract and watch-list configuration checks. The library also builds
and tests without default features. Clippy and the runnable custom-adapter
example pass.

All 29 existing product outputs match the preceding release. The common
adapter requirements produce the same 33 watched keys, including Clock.
These are offline compatibility checks; they do not establish live coverage.
