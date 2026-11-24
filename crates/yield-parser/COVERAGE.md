# Yield parser coverage

Plan parser work by program, account layout, and calculation method. Reuse a
decoder across markets that use the same layout. A protocol can require several
decoders. A lending loop can use lending, collateral, and pricing inputs from
more than one protocol. Use the program and account type to select the layout.

## Current parser outputs

| Protocol | Supported account | Output |
| --- | --- | --- |
| Kamino | Lending reserve, bound to its market and mints | Asset units per collateral share from the reserve's liquidity and share supply. [Kamino Lending SDK](https://github.com/Kamino-Finance/klend-sdk) |
| Jupiter | Main Earn lending account, bound to its reserve and mints | Stored asset units per Earn share. The stored conversion can include rewards that accrued to the share. [Jupiter Lend architecture](https://github.com/jup-ag/docs/blob/main/lend/architecture.mdx) |

These are account values at one observation. A trailing return needs at least
two comparable observations in the same asset unit. A current lending rate,
a posted fixed rate, a reward estimate, and a trailing return each need their
own method and provenance. The separate snapshot collector has Kamino and
Jupiter borrow APR adapters.

The live batch parser also reads the scoped Save and Project 0 share values,
Elemental posted APR, Hylo share value, and Meteora Dynamic Vault share value.
These account adapters need the exact bytes of one Bank. Meteora uses the
Clock from that Bank to calculate how much reported profit remains locked.
[Live account batch](README.md#parse-live-account-batches)

Ledger replay can supply exact checkpoints for Jupiter Earn and Liquidity
events and for Kamino's reserve exchange-rate getter. Earn events need a
proved pool binding to enter a market history. Ordinary Kamino and Save
deposit or redemption amounts reflect execution and rounding. Project 0's
accrual event records changes such as interest and fees. Preserve those
execution facts separately from exact share-price checkpoints.
[Kamino getter](https://github.com/Kamino-Finance/klend/blob/686913ee421e0c5f3dc44c4c8344461ee0e4fb89/programs/klend/src/handlers/handler_calculate_ctoken_exchange_rate.rs)
· [Save reserve math](https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs)
· [Project 0 events](https://github.com/0dotxyz/marginfi-v2/blob/main/programs/marginfi/src/events.rs)

## Source map

A catalogue sample on 22 November 2026 had 435 rows and 46 protocol labels.
The 435 rows contain 190 plain positions, 92 modeled loops, 81 vaults, and
72 liquidity pools. Protocol labels can overlap across rows. The source map
lists each label once under its primary data dependency. The source map includes planned extensions. The seven supported protocol
families and their scoped account layouts are listed above and in the
[protocol registry](../../config/protocols.json).

| Protocol labels | Data needed |
| --- | --- |
| Kamino | Reuse the reserve decoder across supported lending reserves. Earn needs vault holdings, allocation accounts, fees, and share supply. Liquidity strategies also need their pool positions. Institutional vaults need loan and collateral values from outside Solana. [Kamino vault program](https://github.com/Kamino-Finance/kvault) · [Institutional commodity vault](https://kamino.com/earn/institutional/kamino-institutional-commodity-yield/overview) |
| Jupiter | Reuse the Earn decoder for compatible accounts. Add layout checks for other markets. Read the Liquidity reserve and rate model for base lending and borrowing. Add reward schedules when the output needs a reward breakdown. [Jupiter Lend architecture](https://github.com/jup-ag/docs/blob/main/lend/architecture.mdx) |
| Save, Project 0 | Read reserve or bank accounts, interest state, and share values. Follow additional program accounts for wrapped positions. [Save reserve source](https://github.com/solendprotocol/solana-program-library/blob/master/token-lending/program/src/state/reserve.rs) · [Project 0 program](https://github.com/0dotxyz/marginfi-v2) |
| Loopscale | Read loan terms, repayments, vault shares, fees, and idle liquidity positions. Identify each underlying strategy in the valuation. [Earn vault architecture](https://docs.loopscale.com/protocol-concepts/earn-vault-architecture) |
| Exponent | Read principal token quote, redemption value, and maturity for yield to maturity. Tranches, managed vaults, and liquidity positions each need their own accounts and underlying valuation. [Exponent rate markets](https://www.exponent.finance/blog/rate-market-clmm-concentrated-liquidity-growth-rwa) |
| Elemental | Read the posted fixed deposit APR and epoch. Track actual payouts separately. [Elemental FAQ](https://docs.elemental.fund/faq) |
| Orca, Raydium, PancakeSwap, Byreal | Read pool fee growth, relevant positions and ranges, reward state, and prices for both assets. Pool fee growth alone describes only one part of position return. |
| Meteora | Read pool state and pool positions by layout. Dynamic Vault share value also needs time-decayed locked profit and LP supply. [Dynamic Vault formulas](https://github.com/MeteoraAg/docs/blob/main/legacy-products/dynamic-vault/formulas.mdx) |
| Hylo | Read Earn Pool assets and share supply for a value in the deposit token. USD value requires a price for that token. [Hylo Earn Pool math](https://github.com/hylo-so/sdk/blob/5071a0eeb74d9c3ddff41cc71021a9ffa22fcc51/hylo-core/src/earn_pool_math.rs) |
| Reflect | Read tranche shares and allocation state. Track the value and provenance of the underlying assets separately. |
| Voltr, Manifest, GMX Solana, DiversiFi, Piggy Bank | Assess each strategy's holdings, price inputs, fees, and share ledger. Where a keeper publishes position value, retain that source in the result. [Voltr vault SDK](https://voltrxyz.github.io/vault-sdk/classes/VoltrClient.html) · [Manifest Destiny](https://manifest.trade/destiny-docs) |
| Perena | Index the published price and tranche shares. Preserve the source and time of the external yield reports that affect the price. [Perena vault accounting](https://perena.gitbook.io/perena/protocol/usd-technical-design/vault-types-and-yield-strategies) |
| Lulo | Track the valuation of the underlying positions on Solana and Ethereum. [Lulo allocation](https://www.lulo.fi/docs/integrated-protocols) |
| Neutral Trade, Synatra | Track the published share value and its update source. Obtain external strategy values for independent verification. [Neutral Trade pricing](https://docs.neutral.trade/neutral-strategy-vaults/vault-protections) |
| OnRe, Figure, Huma, DAWN, Midas, Obligate, Solayer | Check the token's redemption formula, issuer marks, distributions, and publication cadence. OnRe's contract supplies its redemption price. The insurance portfolio uses separate valuation inputs. [OnRe token configuration](https://docs.onre.finance/technical-resources/token-configuration-and-reference) · [OnRe transparency](https://docs.onre.finance/introduction/transparency-dashboard) |
| Ethena, Apyx, Nest, Re, USD.AI, Maple, Centrifuge, Ondo | Identify the source chain and valuation authority for each asset. Preserve source-chain exchange rates, bridge identity, and issuer marks where applicable. |
| Apollo, Superstate, BlackRock, VanEck, xStocks | Track the issuer or fund NAV, corporate actions, and distributions for each share class. Solana token balances can identify holdings. |
| Solstice, StandX, Unitas | Check the share-price or distribution contract and its inputs by token. Identify any report that represents positions outside Solana. |

## Order of work

1. Replay supported ledger fields. Use two matched measurements from the
   same market and method to calculate return over their observed interval.
   Keep slot, blockhash, time, and transaction position with each observation.
   Use verified account snapshots where the ledger lacks a required value.
2. Expand to the base lending and vault candidates. The sample had 100 Kamino
   reserves, 8 Jupiter Earn rows, 11 Save reserves, 8 Project 0 banks, and 34
   noninstitutional Kamino Earn vaults. These 161 rows are candidate scope. The
   8 Elemental rows are a separate target for posted rates.
3. Add Loopscale, tranche and fixed-rate data, then concentrated liquidity and
   managed vaults. Add cross-chain and issuer adapters where a Solana account
   carries a value that depends on another source.

An admitted block stream can supply historical measurements when transactions
publish them. State between those observations requires independently
verified account state or a replay of all changes from a known initial state.
For incentive or issuer data, retain the original source, observation time,
and calculation basis.
