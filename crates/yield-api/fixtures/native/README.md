# Public native account fixtures

These public account bytes were copied without change from the local Git object
`ef8ead98164714dcd96983e3b0e1d1614fcc2ace` (15 October 2026), under
`services/indexer/fixtures/lending-markets/`. They are offline layout and rate
checks. They do not prove the current account state or a live service result.

| File | Decoded bytes | SHA-256 of decoded bytes |
| --- | ---: | --- |
| `kamino-reserve-usdg-main.b64` | 8624 | `d7e380cad4c379a520ce02652b312dae62df7b733141184c97cc3f25ffbb81ee` |
| `jupiter-reserve-usdg.b64` | 192 | `1a50052cd933f50566df0d83129accda85b346d2938c649ac3f6a540074ee8b3` |
| `jupiter-rate-model-usdg.b64` | 53 | `854d0f5f5d20bd4b6fd4ade18845c354e4e2e38f97d7f2d68eed68106d4237cd` |

The configured public accounts are:

- USDG mint: `2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH`.
- Kamino reserve: `ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp`.
- Jupiter reserve: `G4KAenCBfybnZLtVe4LdNrmEtQoW3x93oTowFJUPSvx6`.
- Jupiter rate model: `6iHHKAK9Mqjn57CVmWe4szAPyTH8s8pniXSj6vWaKW5r`.

The local layout readers are [Kamino](../../src/sources/onchain/kamino.rs)
and [Jupiter](../../src/sources/onchain/jupiter.rs). The snapshot tests construct a minimal SPL
mint with six decimals in memory. That mint fixture is synthetic, not a recorded
USDG mint account. Tests that change an account byte are also synthetic.

The Kamino fixture has reserve version 1, TrueApr basis 1, reserve slot
452570160, and reserve timestamp 1790933433. Its exact curve rate plus the host
fixed fee, rounded down to APR parts per billion, is 54056439. Legacy basis 0,
term debt, penalties, stale state, and unsupported layouts fail the reader.

The Jupiter fixture uses rate-model version 2. Its reserve timestamp is
1790933281. The method uses the stored exchange prices, integer raw-unit
balances, both interest-free balances, and the native two-kink curve. The result
rounded down to APR parts per billion is 54781022. This method does not project
exchange-price accrual to a later time or apply a vault magnifier. It does not
add the lender interest fee to the borrow rate.
The reader rejects a stored reserve update older than 180 seconds at the chain
Clock. A later Clock cannot renew an unchanged old reserve indefinitely.
For both adapters, the observation time remains the original reserve update
time. Evidence records the actual finalized snapshot slot. A repeated account
read does not renew the rate input time.

The readers and their arithmetic were written for this MIT service. No API
rate, token price, supply yield, assumed slot duration, or compounding convention
is part of these fixtures or these methods.
