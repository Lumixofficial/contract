use crate::{mul_div_ceil, mul_div_floor, MathError, PRICE_SCALE};

/// Fully-backed writer collateral.
///
/// Calls are collateralized in base units. Puts are collateralized in quote
/// units at the strike price.
pub fn option_writer_collateral(
    amount: i128,
    strike: i128,
    is_call: bool,
) -> Result<i128, MathError> {
    if amount <= 0 || strike <= 0 {
        return Err(MathError::Invalid);
    }
    if is_call {
        Ok(amount)
    } else {
        mul_div_ceil(amount, strike, PRICE_SCALE)
    }
}

/// European intrinsic-value payout in the writer's collateral asset.
///
/// An ITM call pays base tokens worth `(spot - strike) * amount` at spot.
/// An ITM put pays the same intrinsic value directly in quote tokens.
/// Flooring favors solvency and guarantees the payout never exceeds the
/// fully-backed writer collateral.
pub fn option_buyer_payout(
    amount: i128,
    strike: i128,
    settlement_price: i128,
    is_call: bool,
) -> Result<i128, MathError> {
    if amount <= 0 || strike <= 0 || settlement_price <= 0 {
        return Err(MathError::Invalid);
    }

    if is_call {
        if settlement_price <= strike {
            return Ok(0);
        }
        let intrinsic = settlement_price
            .checked_sub(strike)
            .ok_or(MathError::Overflow)?;
        mul_div_floor(amount, intrinsic, settlement_price)
    } else {
        if settlement_price >= strike {
            return Ok(0);
        }
        let intrinsic = strike
            .checked_sub(settlement_price)
            .ok_or(MathError::Overflow)?;
        mul_div_floor(amount, intrinsic, PRICE_SCALE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SCALAR_7;

    #[test]
    fn call_payout_is_paid_in_base() {
        let amount = 10 * SCALAR_7;
        let strike = 2 * PRICE_SCALE;
        let spot = 4 * PRICE_SCALE;
        assert_eq!(
            option_writer_collateral(amount, strike, true).unwrap(),
            amount
        );
        assert_eq!(
            option_buyer_payout(amount, strike, spot, true).unwrap(),
            5 * SCALAR_7
        );
    }

    #[test]
    fn put_payout_is_paid_in_quote() {
        let amount = 10 * SCALAR_7;
        let strike = 4 * PRICE_SCALE;
        let spot = PRICE_SCALE;
        assert_eq!(
            option_writer_collateral(amount, strike, false).unwrap(),
            40 * SCALAR_7
        );
        assert_eq!(
            option_buyer_payout(amount, strike, spot, false).unwrap(),
            30 * SCALAR_7
        );
    }

    #[test]
    fn out_of_money_and_at_money_pay_zero() {
        let amount = SCALAR_7;
        assert_eq!(
            option_buyer_payout(amount, PRICE_SCALE, PRICE_SCALE, true).unwrap(),
            0
        );
        assert_eq!(
            option_buyer_payout(amount, PRICE_SCALE, PRICE_SCALE / 2, true).unwrap(),
            0
        );
        assert_eq!(
            option_buyer_payout(amount, PRICE_SCALE, PRICE_SCALE, false).unwrap(),
            0
        );
        assert_eq!(
            option_buyer_payout(amount, PRICE_SCALE, PRICE_SCALE * 2, false).unwrap(),
            0
        );
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert!(option_writer_collateral(0, PRICE_SCALE, true).is_err());
        assert!(option_writer_collateral(1, 0, false).is_err());
        assert!(option_buyer_payout(1, PRICE_SCALE, 0, true).is_err());
    }
}
