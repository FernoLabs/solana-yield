# Protocol coverage

The registry lists 46 protocol labels from the 22 November 2026 catalogue
sample. It is a work list, not a claim that every product has a native parser.
The sample had 435 rows: 190 plain positions, 92 modeled loops, 81 vaults, and
72 liquidity pools. One product can depend on several protocols.

The machine-readable source is [config/protocols.json](../config/protocols.json).
The API exposes the same registry at `GET /v1/protocols`.

## Status fields

- `implemented`: this repository has a parser for at least one account or event
  layout. Other layouts under the same protocol can remain unsupported.
- `planned`: native source and calculation work is still required.
- `external`: valuation can depend on another chain, an issuer, a keeper report,
  or another value source. No native parser is claimed here.
- `historical_ledger`: a saved historical ledger test observed this protocol.
  This does not prove a current live feed.
- `fixture_only` and `synthetic_only`: local test data validates the stated
  paths. A successful current live account run is still required.
- `not_verified`: no native live proof is recorded for this scope.

`ledger_support` and `account_support` contain only implemented measurements.
`required_inputs` describes the next data or parser work. An empty support list
means that the registry does not claim a parser for that source.

## Implemented scope

| Protocol | Implemented measurement | Evidence limit |
| --- | --- | --- |
| Kamino | Stored reserve assets per collateral share; reserve getter return | Account and getter fixtures; no Kamino observation in the saved 128-block archive |
| Jupiter | Stored Earn share value; bound Earn event prices; Liquidity event prices and reported borrow APR | Historical Liquidity events; no Earn or rate-update event in the saved archive |
| Save | Stored reserve assets per collateral share, net of reserve fees | 11 products tested with synthetic account-feed frames |
| Project 0 | Stored asset share value for a native lending bank | 8 products tested with synthetic account-feed frames; wrapped positions excluded |
| Elemental | Posted simple annual rate | 8 products tested with synthetic account-feed frames; payout history excluded |
| Hylo | hyUSD per eHYUSD share before withdrawal fees | One product tested with synthetic account-feed frames; USD price excluded |
| Meteora | Unlocked USDT per Dynamic Vault LP share at the Bank Clock | One product tested with synthetic account-feed frames; other pool types excluded |

The five account-batch protocols use 33 account keys for 29 configured products,
including the Clock. A fresh Bank can contain an old protocol update. Keep both
times in the result.

## Full work list

| Protocol | Parser status | Next source or calculation work |
| --- | --- | --- |
| Kamino | implemented | Extend account collection to the monitored reserves. Add Earn vault holdings, fees, and supply before valuing vault shares. |
| Jupiter | implemented | Add all required reserve and rate-model accounts for each selected market. Keep borrowing cost separate from supplier return. |
| Save | implemented | Supply verified reserve accounts and the Clock from one Bank. |
| Project 0 | implemented | Supply verified bank accounts and the Clock from one Bank. Wrapped bank positions require additional layouts. |
| Loopscale | planned | Read loan terms, repayments, vault shares, fees, and idle liquidity positions. Identify each underlying strategy in the valuation. |
| Exponent | planned | Read principal token quote, redemption value, and maturity for yield to maturity. Tranches, managed vaults, and liquidity positions each need their own accounts and underlying valuation. |
| Elemental | implemented | Supply verified fixed-deposit pool accounts and the Clock from one Bank. Track actual distributions separately. |
| Orca | planned | Read pool fee growth, relevant positions and ranges, reward state, and prices for both assets. Pool fee growth alone describes only one part of position return. |
| Raydium | planned | Read pool fee growth, relevant positions and ranges, reward state, and prices for both assets. Pool fee growth alone describes only one part of position return. |
| PancakeSwap | planned | Read pool fee growth, relevant positions and ranges, reward state, and prices for both assets. Pool fee growth alone describes only one part of position return. |
| Byreal | planned | Read pool fee growth, relevant positions and ranges, reward state, and prices for both assets. Pool fee growth alone describes only one part of position return. |
| Meteora | implemented | Supply verified Dynamic Vault, LP mint, and Clock from one Bank. Pool and liquidity-position returns need separate layouts. |
| Hylo | implemented | Supply verified pool config, Earn Pool, share mint, and Clock from one Bank. A USD value also needs a hyUSD price. |
| Reflect | planned | Read tranche shares and allocation state. Track the value and provenance of the underlying assets separately. |
| Voltr | planned | Assess each strategy's holdings, price inputs, fees, and share ledger. Where a keeper publishes position value, retain that source in the result. |
| Manifest | planned | Assess each strategy's holdings, price inputs, fees, and share ledger. Where a keeper publishes position value, retain that source in the result. |
| GMX Solana | planned | Assess each strategy's holdings, price inputs, fees, and share ledger. Where a keeper publishes position value, retain that source in the result. |
| DiversiFi | planned | Assess each strategy's holdings, price inputs, fees, and share ledger. Where a keeper publishes position value, retain that source in the result. |
| Piggy Bank | planned | Assess each strategy's holdings, price inputs, fees, and share ledger. Where a keeper publishes position value, retain that source in the result. |
| Perena | external | Index the published price and tranche shares. Preserve the source and time of the external yield reports that affect the price. |
| Lulo | external | Track the valuation of the underlying positions on Solana and Ethereum. |
| Neutral Trade | external | Track the published share value and its update source. Obtain external strategy values for independent verification. |
| Synatra | external | Track the published share value and its update source. Obtain external strategy values for independent verification. |
| OnRe | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| Figure | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| Huma | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| DAWN | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| Midas | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| Obligate | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| Solayer | external | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. |
| Ethena | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Apyx | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Nest | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Re | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| USD.AI | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Maple | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Centrifuge | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Ondo | external | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Apollo | external | Track the issuer or fund NAV, corporate actions, and distributions for each share class. Solana token balances can identify holdings. |
| Superstate | external | Track the issuer or fund NAV, corporate actions, and distributions for each share class. Solana token balances can identify holdings. |
| BlackRock | external | Track the issuer or fund NAV, corporate actions, and distributions for each share class. Solana token balances can identify holdings. |
| VanEck | external | Track the issuer or fund NAV, corporate actions, and distributions for each share class. Solana token balances can identify holdings. |
| xStocks | external | Track the issuer or fund NAV, corporate actions, and distributions for each share class. Solana token balances can identify holdings. |
| Solstice | external | Check the share-price or distribution contract and its inputs by token. Identify any report that represents positions outside Solana. |
| StandX | external | Check the share-price or distribution contract and its inputs by token. Identify any report that represents positions outside Solana. |
| Unitas | external | Check the share-price or distribution contract and its inputs by token. Identify any report that represents positions outside Solana. |

## Source references

- [Kamino Lend source](https://github.com/Kamino-Finance/klend) and
  [Kamino vault source](https://github.com/Kamino-Finance/kvault)
- [Jupiter Lend source](https://github.com/jup-ag/jupiter-lend)
- [Save reserve source](https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs)
- [Project 0 source](https://github.com/0dotxyz/marginfi-v2)
- [Elemental documentation](https://docs.elemental.fund/faq)
- [Hylo SDK](https://github.com/hylo-so/sdk)
- [Meteora Dynamic Vault source](https://github.com/MeteoraAg/vault-sdk)

Source selection is part of each adapter review. Pin the exact revision when
adding or changing a layout. See [the contribution guide](../CONTRIBUTING.md).
