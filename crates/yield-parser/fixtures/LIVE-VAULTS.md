# Live vault watch list

`live-vaults.json` uses the eight Elemental fixed deposits, one Hylo Earn
Pool, and one Meteora Dynamic Vault in the 23 November 2026 Paris yield
catalogue. The market keys identify accounts to verify in a Bank snapshot.

- Elemental uses the two program layouts in the established catalogue reader.
  The stored annual rate uses a denominator of one billion. The account
  supplies the deposit token and amount unit. [Elemental FAQ](https://docs.elemental.fund/faq)
- The three `live-hylo-*.bin` files contain public account data from mainnet
  slot 449259126 on 22 September 2026. They come from the mainnet Hylo
  fixtures used by FireStrategy's indexer. The Earn Pool ratio uses custody
  balance and share mint supply in hyUSD units. [Hylo math](https://github.com/hylo-so/sdk/blob/5071a0eeb74d9c3ddff41cc71021a9ffa22fcc51/hylo-core/src/earn_pool_math.rs)
- Meteora's vault data API lists the USDT vault and LP mint. The supported
  versions use either the SPL mint supply or the vault's tracked supply.
  The decoder subtracts remaining locked profit with the same Bank's Clock.
  [Vault catalogue](https://merv2-api.meteora.ag/vault_info) · [Account layout](https://github.com/MeteoraAg/vault-sdk/blob/main/programs/vault/src/state.rs)
