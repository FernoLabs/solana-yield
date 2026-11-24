# Roadmap

The repository can parse supported data and serve collected observations. It
does not yet provide native live yields for every protocol in the registry.
The order below reduces the gap between the existing components and that goal.

## 1. Complete the native ingestion path

- Feed verified Blockzilla ledger and account observations into one persistent
  observation store and the API.
- Retain exact ratios, asset units, method, source, Bank, transaction position,
  observation time, and protocol update time.
- Keep the five-second Bank age check. Record gaps and source changes instead
  of silently continuing with stale data.
- Keep RPC disabled in the native monitor. Leave existing API/RPC collectors as
  separate explicit options.

The stream and account observer are separate from the API store today. The
first end-to-end test must show that accepted native observations reach the API
with the same evidence and availability state.

## 2. Prove live coverage for implemented layouts

Start with the 29-product account watch list and the supported Jupiter ledger
records. Add configured Kamino and Jupiter account sources to the native account
path. Report acceptance and missing data by product and measurement.

Do not infer complete market coverage from one supported layout. Compare a
small set of stored values with exact-Bank reads. Test stale input, reconnects,
source changes, and unavailable accounts before extending the watch list.

## 3. Add calculated returns and withdrawal checks

Store at least two comparable share values over a stated period. Calculate the
period return from exact ratios before any annualization. Define the treatment
of fees, deposits, withdrawals, rewards, rebasing, and asset prices for each
method. Keep posted APR and borrowing cost separate.

The [withdrawal-check design](redemption-validation.md) proposes optional
local execution to validate net redeemable value where a direct account value
is unavailable or needs an independent check. It remains separate from the
parser and requires complete verified execution inputs.

Publish the input observations and calculation method with each result. Avoid
short-window annualized values until the sampling and rounding error are known.

## 4. Expand protocol coverage

Use [the 46-protocol registry](coverage.md). Extend the existing Kamino and
Jupiter layouts across compatible markets first. Then add base vault and
lending sources such as Loopscale, followed by fixed-term/tranche products,
concentrated liquidity positions, and managed strategies.

Liquidity-pool fees alone are not total position return. Managed strategies
need holdings, fees, allocation, and price sources. Issuer and cross-chain
products need explicit external value provenance.

## 5. Prepare a stable public service

Add versioned data migrations, retention and archive policy, continuous
availability measurements, and deployment examples. Document the access and
rate-limit policy before operating a public endpoint. Publish a stable API only
after the native ingestion and calculation contracts have acceptance tests.
