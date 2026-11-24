//! Historical Jupiter Earn and Liquidity observations from Anchor event bytes.
//!
//! Layouts come from the [Earn IDL] and [Liquidity IDL] for Jupiter Lend 0.1.9.
//! The [Earn update] stores both prices before emitting the event. The
//! [Liquidity update] reports the prices, borrow APR in basis points, and
//! utilization in basis points. A [Liquidity operation] records the prices
//! that the operation stored. It has no APR field. Both programs use a price
//! scale of 10^12.
//!
//! [Earn IDL]: https://github.com/jup-ag/jupiter-lend/blob/7960a14a5a760abc41db3ed1395dc5cb56a1f8db/target/idl/lending.json
//! [Liquidity IDL]: https://github.com/jup-ag/jupiter-lend/blob/7960a14a5a760abc41db3ed1395dc5cb56a1f8db/target/idl/liquidity.json
//! [Earn update]: https://github.com/Instadapp/fluid-solana-programs/blob/626b177f235224bc5d074d39439cd2558f542886/programs/lending/src/utils/helpers.rs#L199-L218
//! [Liquidity update]: https://github.com/Instadapp/fluid-solana-programs/blob/626b177f235224bc5d074d39439cd2558f542886/programs/liquidity/src/state/token_reserve.rs#L103-L117
//! [Liquidity operation]: https://github.com/Instadapp/fluid-solana-programs/blob/626b177f235224bc5d074d39439cd2558f542886/programs/liquidity/src/module/user.rs#L161-L168

use crate::{key, LedgerMeasurement, ParseError};

pub const EARN_PROGRAM: &str = "jup3YeL8QhtSx1e253b2FDvsMNC87fDrgQZivbrndc9";
pub const LIQUIDITY_PROGRAM: &str = "jupeiUmn818Jg1ekPURTpr4mFo29p46vygyykFJ3wZC";

const EARN_UPDATE: [u8; 8] = [222, 11, 113, 60, 147, 15, 68, 217];
const LIQUIDITY_UPDATE: [u8; 8] = [190, 194, 69, 204, 30, 86, 181, 163];
const LIQUIDITY_OPERATE: [u8; 8] = [180, 8, 81, 71, 19, 132, 173, 8];

/// The pool identity carried by a reviewed Earn instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ledger", derive(serde::Serialize))]
pub struct EarnMarket {
    #[cfg_attr(
        feature = "ledger",
        serde(serialize_with = "crate::ledger::serialize_key")
    )]
    pub pool: [u8; 32],
    #[cfg_attr(
        feature = "ledger",
        serde(serialize_with = "crate::ledger::serialize_key")
    )]
    pub asset_mint: [u8; 32],
    #[cfg_attr(
        feature = "ledger",
        serde(serialize_with = "crate::ledger::serialize_key")
    )]
    pub share_mint: [u8; 32],
    #[cfg_attr(
        feature = "ledger",
        serde(serialize_with = "crate::ledger::serialize_key")
    )]
    pub liquidity_reserve: [u8; 32],
}

/// Read identities from an instruction of the main Earn program. Account keys
/// must be resolved in the instruction's declared order, including optional
/// placeholders. Attach this identity only to an event proved to come from
/// this exact successful instruction.
pub fn earn_market(data: &[u8], accounts: &[&str]) -> Result<Option<EarnMarket>, ParseError> {
    let (data_len, account_count, [pool, asset, share, reserve]) = match data.get(..8) {
        // Deposit and mint use the Deposit context.
        Some([242, 35, 198, 137, 82, 225, 242, 182] | [51, 57, 225, 47, 182, 146, 137, 166]) => {
            (16, 17, [5, 3, 6, 7])
        }
        // deposit_with_min_amount_out and mint_with_max_assets.
        Some([116, 144, 16, 97, 118, 109, 40, 119] | [6, 94, 69, 122, 30, 179, 146, 171]) => {
            (24, 17, [5, 3, 6, 7])
        }
        // Withdraw and redeem use the Withdraw context.
        Some([183, 18, 70, 156, 148, 109, 161, 34] | [184, 12, 86, 149, 70, 196, 97, 225]) => {
            (16, 18, [4, 5, 6, 7])
        }
        // withdraw_with_max_shares_burn and redeem_with_min_amount_out.
        Some([47, 197, 183, 171, 239, 18, 245, 171] | [235, 189, 237, 56, 166, 180, 184, 149]) => {
            (24, 18, [4, 5, 6, 7])
        }
        // update_rate reads the pool, mints, reserve, and reward model.
        Some([24, 225, 53, 189, 72, 212, 225, 178]) => (8, 5, [0, 1, 2, 3]),
        _ => return Ok(None),
    };
    if data.len() != data_len || accounts.len() != account_count {
        return Err(ParseError::UnsupportedLayout);
    }
    let identities = [
        key(accounts[pool])?,
        key(accounts[asset])?,
        key(accounts[share])?,
        key(accounts[reserve])?,
    ];
    for (index, identity) in identities.iter().enumerate() {
        if *identity == [0; 32] || identities[..index].contains(identity) {
            return Err(ParseError::AccountMismatch);
        }
    }
    let [pool, asset_mint, share_mint, liquidity_reserve] = identities;
    Ok(Some(EarnMarket {
        pool,
        asset_mint,
        share_mint,
        liquidity_reserve,
    }))
}

/// Decode an event from a caller-verified emitter and successful invocation.
///
/// Earn's update event has no market or mint. A caller needs the exact
/// instruction and resolved accounts to associate it with a market. To
/// establish historical coverage, also verify the transaction, block,
/// log completeness, and chain ordering outside this function.
pub fn decode(emitter: &str, event: &[u8]) -> Result<Option<LedgerMeasurement>, ParseError> {
    match emitter {
        EARN_PROGRAM if matches_event(event, &EARN_UPDATE, 24)? => {
            let token_exchange_price =
                u64::from_le_bytes(event[8..16].try_into().expect("validated Earn event size"));
            let liquidity_exchange_price =
                u64::from_le_bytes(event[16..24].try_into().expect("validated Earn event size"));
            if token_exchange_price == 0 || liquidity_exchange_price == 0 {
                return Err(ParseError::InvalidValue);
            }
            Ok(Some(LedgerMeasurement::JupiterEarnExchangePrices {
                token_exchange_price,
                liquidity_exchange_price,
            }))
        }
        LIQUIDITY_PROGRAM if matches_event(event, &LIQUIDITY_UPDATE, 76)? => {
            let token = event[8..40]
                .try_into()
                .expect("validated Liquidity event size");
            let supply_exchange_price = u128::from_le_bytes(
                event[40..56]
                    .try_into()
                    .expect("validated Liquidity event size"),
            );
            let borrow_exchange_price = u128::from_le_bytes(
                event[56..72]
                    .try_into()
                    .expect("validated Liquidity event size"),
            );
            let borrow_rate_bps = u16::from_le_bytes(
                event[72..74]
                    .try_into()
                    .expect("validated Liquidity event size"),
            );
            let utilization_bps = u16::from_le_bytes(
                event[74..76]
                    .try_into()
                    .expect("validated Liquidity event size"),
            );
            if token == [0; 32] || supply_exchange_price == 0 || borrow_exchange_price == 0 {
                return Err(ParseError::InvalidValue);
            }
            Ok(Some(LedgerMeasurement::JupiterLiquidityExchangePrices {
                token,
                supply_exchange_price,
                borrow_exchange_price,
                borrow_rate_bps,
                utilization_bps,
            }))
        }
        LIQUIDITY_PROGRAM if matches_event(event, &LIQUIDITY_OPERATE, 184)? => {
            let token = event[40..72]
                .try_into()
                .expect("validated Liquidity operation size");
            let supply_exchange_price = u64::from_le_bytes(
                event[168..176]
                    .try_into()
                    .expect("validated Liquidity operation size"),
            );
            let borrow_exchange_price = u64::from_le_bytes(
                event[176..184]
                    .try_into()
                    .expect("validated Liquidity operation size"),
            );
            if token == [0; 32] || supply_exchange_price == 0 || borrow_exchange_price == 0 {
                return Err(ParseError::InvalidValue);
            }
            Ok(Some(LedgerMeasurement::JupiterLiquidityOperationPrices {
                token,
                supply_exchange_price,
                borrow_exchange_price,
            }))
        }
        _ => Ok(None),
    }
}

fn matches_event(event: &[u8], discriminator: &[u8; 8], size: usize) -> Result<bool, ParseError> {
    if event.starts_with(discriminator) {
        if event.len() != size {
            return Err(ParseError::UnsupportedLayout);
        }
        return Ok(true);
    }
    if !event.is_empty() && discriminator.starts_with(event) {
        return Err(ParseError::UnsupportedLayout);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn earn_event() -> Vec<u8> {
        [
            EARN_UPDATE.as_slice(),
            &1_067_222_452_096_u64.to_le_bytes(),
            &1_041_771_553_914_u64.to_le_bytes(),
        ]
        .concat()
    }

    fn liquidity_event() -> Vec<u8> {
        [
            LIQUIDITY_UPDATE.as_slice(),
            &[27; 32],
            &1_041_771_553_914_u128.to_le_bytes(),
            &1_201_898_984_621_u128.to_le_bytes(),
            &15_629_u16.to_le_bytes(),
            &10_052_u16.to_le_bytes(),
        ]
        .concat()
    }

    fn operation_event() -> [u8; 184] {
        let mut event = [0; 184];
        event[..8].copy_from_slice(&LIQUIDITY_OPERATE);
        event[8..40].fill(37);
        event[40..72].fill(27);
        event[168..176].copy_from_slice(&1_041_771_553_914_u64.to_le_bytes());
        event[176..].copy_from_slice(&1_201_898_984_621_u64.to_le_bytes());
        event
    }

    #[test]
    fn decodes_an_exact_earn_price_without_adding_market_identity() {
        let event = earn_event();
        assert!(matches!(
            decode(EARN_PROGRAM, &event).unwrap(),
            Some(LedgerMeasurement::JupiterEarnExchangePrices {
                token_exchange_price: 1_067_222_452_096,
                liquidity_exchange_price: 1_041_771_553_914,
            })
        ));
    }

    #[test]
    fn decodes_liquidity_prices_rates_and_utilization_without_narrowing() {
        let event = liquidity_event();
        let Some(LedgerMeasurement::JupiterLiquidityExchangePrices {
            token,
            supply_exchange_price: 1_041_771_553_914,
            borrow_exchange_price: 1_201_898_984_621,
            borrow_rate_bps: 15_629,
            utilization_bps: 10_052,
        }) = decode(LIQUIDITY_PROGRAM, &event).unwrap()
        else {
            panic!("expected Liquidity update");
        };
        assert_eq!(token, [27; 32]);
    }

    #[test]
    fn decodes_operation_prices_as_a_separate_measurement() {
        let mut event = operation_event();
        let Some(LedgerMeasurement::JupiterLiquidityOperationPrices {
            token,
            supply_exchange_price: 1_041_771_553_914,
            borrow_exchange_price: 1_201_898_984_621,
        }) = decode(LIQUIDITY_PROGRAM, &event).unwrap()
        else {
            panic!("expected Liquidity operation prices");
        };
        assert_eq!(token, [27; 32]);
        event[176..].fill(0);
        assert!(matches!(
            decode(LIQUIDITY_PROGRAM, &event),
            Err(ParseError::InvalidValue)
        ));
    }

    #[test]
    fn rejects_truncated_or_extended_known_layouts_and_ignores_other_emitters() {
        for (program, mut event) in [
            (EARN_PROGRAM, earn_event()),
            (LIQUIDITY_PROGRAM, liquidity_event()),
            (LIQUIDITY_PROGRAM, operation_event().to_vec()),
        ] {
            for prefix_len in 1..event.len() {
                assert!(matches!(
                    decode(program, &event[..prefix_len]),
                    Err(ParseError::UnsupportedLayout)
                ));
            }
            event.push(0);
            assert!(decode(program, &event).is_err());
            assert!(decode("11111111111111111111111111111111", &event)
                .unwrap()
                .is_none());
        }
        assert!(decode(EARN_PROGRAM, &liquidity_event()).unwrap().is_none());
        assert!(decode(LIQUIDITY_PROGRAM, &earn_event()).unwrap().is_none());
    }

    #[test]
    fn rejects_a_default_token_or_zero_price_but_accepts_zero_rate() {
        let mut earn = earn_event();
        earn[8..16].fill(0);
        assert!(matches!(
            decode(EARN_PROGRAM, &earn),
            Err(ParseError::InvalidValue)
        ));
        let mut liquidity = liquidity_event();
        liquidity[72..].fill(0);
        assert!(matches!(
            decode(LIQUIDITY_PROGRAM, &liquidity).unwrap(),
            Some(LedgerMeasurement::JupiterLiquidityExchangePrices {
                borrow_rate_bps: 0,
                utilization_bps: 0,
                ..
            })
        ));
        liquidity[8..40].fill(0);
        assert!(matches!(
            decode(LIQUIDITY_PROGRAM, &liquidity),
            Err(ParseError::InvalidValue)
        ));
    }

    #[test]
    fn binds_reviewed_earn_instruction_layouts_in_declared_account_order() {
        let keys: Vec<_> = (1..=18)
            .map(|byte| bs58::encode([byte; 32]).into_string())
            .collect();
        let accounts: Vec<_> = keys.iter().map(String::as_str).collect();
        for (discriminator, data_len, count, [pool, asset, share, reserve]) in [
            ([242, 35, 198, 137, 82, 225, 242, 182], 16, 17, [6, 4, 7, 8]),
            ([184, 12, 86, 149, 70, 196, 97, 225], 16, 18, [5, 6, 7, 8]),
            (
                [235, 189, 237, 56, 166, 180, 184, 149],
                24,
                18,
                [5, 6, 7, 8],
            ),
            ([24, 225, 53, 189, 72, 212, 225, 178], 8, 5, [1, 2, 3, 4]),
        ] {
            let mut data = vec![0; data_len];
            data[..8].copy_from_slice(&discriminator);
            let binding = earn_market(&data, &accounts[..count]).unwrap().unwrap();
            assert_eq!(binding.pool, [pool; 32]);
            assert_eq!(binding.asset_mint, [asset; 32]);
            assert_eq!(binding.share_mint, [share; 32]);
            assert_eq!(binding.liquidity_reserve, [reserve; 32]);
        }
    }

    #[test]
    fn refuses_missing_duplicate_and_invalid_account_identities() {
        let update_rate = [24, 225, 53, 189, 72, 212, 225, 178];
        assert!(earn_market(&[0; 8], &[]).unwrap().is_none());
        assert!(matches!(
            earn_market(&update_rate, &[]),
            Err(ParseError::UnsupportedLayout)
        ));
        let keys: Vec<_> = (1..=5)
            .map(|byte| bs58::encode([byte; 32]).into_string())
            .collect();
        let mut accounts: Vec<_> = keys.iter().map(String::as_str).collect();
        accounts[0] = accounts[1];
        assert!(matches!(
            earn_market(&update_rate, &accounts),
            Err(ParseError::AccountMismatch)
        ));
        accounts[0] = "invalid base58 account";
        assert!(matches!(
            earn_market(&update_rate, &accounts),
            Err(ParseError::InvalidAccount)
        ));
    }
}
