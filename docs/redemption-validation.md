# Withdrawal value and return measurements

## Decision

Add an optional withdrawal check outside the offline parser. Use it to validate
an account-derived share value and as a fallback for a protocol without a direct
share-value formula. The execution service is not implemented in this change.
The parser must remain usable without a simulator, network, or database.

Keep four results separate:

| Result | Meaning |
| --- | --- |
| Stored share value | The protocol's account value at a stated Bank and update time |
| Redemption value | Underlying assets returned for a specified share quantity, with stated fees |
| Exit conditions | Immediate capacity, waiting period, and any secondary-market route |
| Period return | Change between comparable values over a stated interval |

A successful withdrawal simulation supplies one value at one Bank. A return
needs comparable observations at different times. Its result also depends on
position size, available liquidity, the route, and the instruction sequence.

## Published reference

[YieldCompass's methodology](https://docs.yieldcompass.fi/realized-apy/methodology)
uses daily exchange-rate observations. It describes deposit/withdrawal
simulation as a fallback when no direct exchange rate is available. It treats
fees and non-earning cooldowns as separate adjustments under a declared
one-exit-per-year assumption. We should retain explicit assumptions rather
than present adjusted figures as direct observations.

Its [exit-liquidity documentation](https://docs.yieldcompass.fi/on-chain-monitoring/exit-liquidity)
separately considers redemption capacity, size-dependent market quotes, and
unstaking delays. These are useful fields for a withdrawal check.

## Proposed execution boundary

1. The collector admits the source, Bank, account bytes, and program versions.
2. A protocol-specific transaction builder describes the withdrawal for a
   fixed share quantity. This builder is separate from the account parser.
3. An isolated execution service runs that instruction sequence against the
   admitted state and discards every state change.
4. A result verifier checks execution status and token-account balance changes.
   It retains the exact output assets and fees, not only log messages.
5. The calculation layer compares the result with the parser output. It stores
   discrepancies and unavailable results with their evidence.

The read-only proof of concept must not make an on-chain deposit to create a
sample position. Use an existing position with verified input state, or a
clearly labelled synthetic position in a local test Bank. A synthetic setup
must not be labelled as an observed user return.

Solana's [`simulateTransaction`](https://solana.com/docs/rpc/http/simulatetransaction)
does not broadcast a transaction, but it is an RPC method. It does not meet our
zero-RPC transport requirement. A later local execution service needs complete
relevant state and compatible program/runtime versions. Compact V2 events alone
do not supply that state. This service remains planned work.

## Minimum quote record

- Protocol, market, asset mint, share mint, and requested raw share quantity.
- Source identity, Bank slot and blockhash, commitment/native-control status,
  Bank time, observation time, and the input account-set digest.
- Program revision or executable hash and instruction/route identity.
- Method: direct formula, protocol quote, local execution, or external quote.
- Net assets returned. Include gross assets and each fee with its asset and
  unit only when separately known; mark unknown components explicitly.
- Capacity at the tested size, waiting period, and whether the exit is immediate.
- Status: available, insufficient liquidity, waiting required, unsupported,
  missing state, or failed execution.
- Assumptions about rewards, prices, rounding, reinvestment, and exit frequency.

Do not subtract a fee again when the measured share value already includes it.
Keep a redemption into the underlying token separate from a swap into USD or
another token. An unclaimed reward is a separate asset until its value and
conversion policy are known.

## Return calculation

For the same quantity of shares, asset unit, method, and comparable state,
let `V0` and `V1` be the measured values at `t0` and `t1`. The period return is
`V1 / V0 - 1`. Any distributions, changes in share quantity, or external cash
flows need their own accounting before this formula can describe a position.

Annualization is a separate calculation with a stated time convention. Keep
short-window annualization, incentives, hypothetical exit costs, and cooldown
adjustments separate from the observed period return.

## Acceptance tests before live use

- Compare a direct formula and local execution for the same Bank and share size.
- Check small and large quantities, rounding, zero output, and fee changes.
- Check low liquidity, queued exits, cooldowns, and failed inner instructions.
- Reject mixed-Bank inputs, unknown program versions, and incomplete state.
- Prove that the executor cannot send transactions or use RPC in native mode.
- Retain original timestamps. Apply the five-second rule to live admission;
  keep historical observations explicitly historical.

Historical execution needs the account state and program versions from the
historical Bank. It cannot use today's state to create an old redemption value.
