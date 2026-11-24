//! Kamino Reserve exchange-price return data from a successful ledger instruction.

use crate::{key, LedgerMeasurement, ParseError};

pub const PROGRAM: &str = crate::kamino::PROGRAM;
pub const CALCULATE_CTOKEN_EXCHANGE_RATE: [u8; 8] = [32, 253, 220, 13, 19, 165, 131, 188];

/// Identify a getter candidate before checking its full layout.
pub fn matches_getter(data: &[u8]) -> bool {
    data.starts_with(&CALCULATE_CTOKEN_EXCHANGE_RATE)
}

/// Decode the result of the pinned Kamino reserve getter. The caller must bind
/// the return data to this successful instruction. Solana stores one return
/// value for the whole transaction, so a transaction can overwrite it.
///
/// `exchange_rate_sf` is Q60 raw liquidity units for one whole cToken
/// (1,000,000 raw collateral units). `liquidity_mint_decimals` describes the
/// liquidity mint. The getter checks that the reserve was refreshed at the
/// execution slot.
///
/// [Pinned getter]: https://github.com/Kamino-Finance/klend/blob/686913ee421e0c5f3dc44c4c8344461ee0e4fb89/programs/klend/src/handlers/handler_calculate_ctoken_exchange_rate.rs
///
/// See the [pinned getter][Pinned getter] for the account and units.
pub fn decode(
    instruction: &[u8],
    accounts: &[&str],
    return_program: &str,
    return_data: &[u8],
) -> Result<Option<LedgerMeasurement>, ParseError> {
    if !matches_getter(instruction) {
        return Ok(None);
    }
    if instruction != CALCULATE_CTOKEN_EXCHANGE_RATE || return_data.len() != 17 {
        return Err(ParseError::UnsupportedLayout);
    }
    let [account] = accounts else {
        return Err(ParseError::AccountMismatch);
    };
    let reserve = key(account)?;
    if return_program != PROGRAM || reserve == [0; 32] {
        return Err(ParseError::AccountMismatch);
    }

    let exchange_rate_sf = u128::from_le_bytes(
        return_data[..16]
            .try_into()
            .expect("validated Kamino return data length"),
    );
    if exchange_rate_sf == 0 {
        return Err(ParseError::InvalidValue);
    }
    Ok(Some(LedgerMeasurement::KaminoReserveExchangePrice {
        reserve,
        exchange_rate_sf,
        liquidity_mint_decimals: return_data[16],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESERVE: &str = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp";
    const OTHER_PROGRAM: &str = "11111111111111111111111111111111";
    const SCALED_AMOUNT: u128 = 1_250_000_u128 << 60;

    fn return_data() -> [u8; 17] {
        let mut bytes = [0_u8; 17];
        bytes[..16].copy_from_slice(&SCALED_AMOUNT.to_le_bytes());
        bytes[16] = 6;
        bytes
    }

    #[test]
    fn retains_the_fixed_point_value_and_the_liquidity_decimals() {
        let result = decode(
            &CALCULATE_CTOKEN_EXCHANGE_RATE,
            &[RESERVE],
            PROGRAM,
            &return_data(),
        )
        .unwrap();
        let Some(LedgerMeasurement::KaminoReserveExchangePrice {
            reserve,
            exchange_rate_sf,
            liquidity_mint_decimals,
        }) = result
        else {
            panic!("expected a Kamino exchange-price observation");
        };
        assert_eq!(reserve, key(RESERVE).unwrap());
        assert_eq!(exchange_rate_sf, SCALED_AMOUNT);
        assert_eq!(liquidity_mint_decimals, 6);
    }

    #[test]
    fn allows_other_instructions_without_interpreting_their_return_data() {
        assert!(decode(&[0; 8], &[], PROGRAM, &return_data())
            .unwrap()
            .is_none());
    }

    #[test]
    fn declines_unbound_or_malformed_quotes() {
        let data = return_data();
        let instruction = CALCULATE_CTOKEN_EXCHANGE_RATE;
        assert!(matches!(
            decode(&instruction, &[RESERVE], OTHER_PROGRAM, &data),
            Err(ParseError::AccountMismatch)
        ));
        assert!(matches!(
            decode(&instruction, &[RESERVE, RESERVE], PROGRAM, &data),
            Err(ParseError::AccountMismatch)
        ));
        assert!(matches!(
            decode(&instruction, &[OTHER_PROGRAM], PROGRAM, &data),
            Err(ParseError::AccountMismatch)
        ));
        assert!(matches!(
            decode(&instruction, &[RESERVE], PROGRAM, &data[..16]),
            Err(ParseError::UnsupportedLayout)
        ));
        let mut extended = instruction.to_vec();
        extended.push(0);
        assert!(matches!(
            decode(&extended, &[RESERVE], PROGRAM, &data),
            Err(ParseError::UnsupportedLayout)
        ));
        assert!(matches!(
            decode(&instruction, &[RESERVE], PROGRAM, &[0; 17]),
            Err(ParseError::InvalidValue)
        ));
    }
}
