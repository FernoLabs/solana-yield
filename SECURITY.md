# Security

## Reporting

For a possible vulnerability, contact a repository maintainer through a private
channel. Do not post credentials, private account data, or an active exploit in
a public issue. Include the affected commit, input requirements, and a minimal
reproduction with synthetic or public data.

## Data and execution boundary

This project reads data. It does not sign or send transactions. A validated
provider frame proves the stated source contract; it is not independent proof
of Solana consensus. Verify and pin the provider identity before a live run.

The HTTP API has no built-in authentication. The examples listen on loopback.
Use an authenticated proxy and explicit access controls before exposing it to
other networks. Do not expose a provider credential to API callers.

Missing, stale, inconsistent, or unsupported data must produce an unavailable
result. Do not loosen a parser check to recover a value from an unknown layout.
Do not treat a transaction request as a successful transfer.

Keep provider tokens in separate local files or environment variables. Keep
runtime databases, raw captures, and local configurations outside Git. Report
files may contain public account addresses and source identifiers; inspect
them before sharing.

The account observer's five-second limit applies to the original Bank
observation time. It does not renew the time of a protocol value stored in that
Bank. Production consumers must choose and enforce their required finality and
source-trust policy.
