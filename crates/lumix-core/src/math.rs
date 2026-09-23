use crate::constants::{BPS, PRICE_SCALE, SCALAR_7};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathError {
    Overflow,
    DivisionByZero,
    Negative,
    Invalid,
}

pub fn require_positive(amount: i128) -> Result<i128, MathError> {
    if amount <= 0 {
        return Err(MathError::Negative);
    }
    Ok(amount)
}

pub fn require_non_negative(amount: i128) -> Result<i128, MathError> {
    if amount < 0 {
        return Err(MathError::Negative);
    }
    Ok(amount)
}

pub fn mul_div_floor(a: i128, b: i128, d: i128) -> Result<i128, MathError> {
    if d == 0 {
        return Err(MathError::DivisionByZero);
    }
    let product = a.checked_mul(b).ok_or(MathError::Overflow)?;
    let quotient = product.checked_div(d).ok_or(MathError::Overflow)?;
    let remainder = product.checked_rem(d).ok_or(MathError::Overflow)?;
    if remainder != 0 && (product < 0) != (d < 0) {
        quotient.checked_sub(1).ok_or(MathError::Overflow)
    } else {
        Ok(quotient)
    }
}

pub fn mul_div_ceil(a: i128, b: i128, d: i128) -> Result<i128, MathError> {
    if d == 0 {
        return Err(MathError::DivisionByZero);
    }
    let product = a.checked_mul(b).ok_or(MathError::Overflow)?;
    let quotient = product.checked_div(d).ok_or(MathError::Overflow)?;
    let remainder = product.checked_rem(d).ok_or(MathError::Overflow)?;
    if remainder != 0 && (product < 0) == (d < 0) {
        quotient.checked_add(1).ok_or(MathError::Overflow)
    } else {
        Ok(quotient)
    }
}

/// Convert a token amount (7 decimals) and a price (14 decimals) into USD (7 decimals).
pub fn to_usd(amount: i128, price: i128) -> Result<i128, MathError> {
    require_non_negative(amount)?;
    require_positive(price)?;
    mul_div_floor(amount, price, PRICE_SCALE)
}

/// Convert USD (7 decimals) into a token amount at `price` (14 decimals).
pub fn from_usd(usd: i128, price: i128) -> Result<i128, MathError> {
    require_non_negative(usd)?;
    require_positive(price)?;
    mul_div_floor(usd, PRICE_SCALE, price)
}

pub fn bps_of_floor(amount: i128, bps: u32) -> Result<i128, MathError> {
    mul_div_floor(amount, i128::from(bps), BPS)
}

pub fn bps_of_ceil(amount: i128, bps: u32) -> Result<i128, MathError> {
    mul_div_ceil(amount, i128::from(bps), BPS)
}

pub fn apply_bps_increase(amount: i128, bps: u32) -> Result<i128, MathError> {
    let add = bps_of_ceil(amount, bps)?;
    amount.checked_add(add).ok_or(MathError::Overflow)
}

pub fn apply_bps_decrease(amount: i128, bps: u32) -> Result<i128, MathError> {
    let sub = bps_of_floor(amount, bps)?;
    amount.checked_sub(sub).ok_or(MathError::Overflow)
}

/// Signed magnitude helper: apply `op` to `abs(x)` then restore sign.
pub fn abs_i128(x: i128) -> Result<i128, MathError> {
    if x == i128::MIN {
        return Err(MathError::Overflow);
    }
    Ok(x.abs())
}

/// Utilization in SCALAR_7 units, capped at 100%.
pub fn utilization(reserved: i128, pool: i128) -> Result<i128, MathError> {
    require_non_negative(reserved)?;
    require_non_negative(pool)?;
    if pool == 0 {
        return Ok(0);
    }
    if reserved >= pool {
        return Ok(SCALAR_7);
    }
    mul_div_ceil(reserved, SCALAR_7, pool)
}

/// Apply a bid/ask spread around a mid price.
/// `spread_bps` is the half-spread. min = mid * (1 - s), max = mid * (1 + s).
pub fn min_max_from_mid(mid: i128, spread_bps: u32) -> Result<(i128, i128), MathError> {
    require_positive(mid)?;
    let min = apply_bps_decrease(mid, spread_bps)?;
    let max = apply_bps_increase(mid, spread_bps)?;
    if min <= 0 {
        return Err(MathError::Invalid);
    }
    Ok((min, max))
}

pub fn mid_price(min: i128, max: i128) -> Result<i128, MathError> {
    require_positive(min)?;
    require_positive(max)?;
    if max < min {
        return Err(MathError::Invalid);
    }
    min.checked_add(max)
        .ok_or(MathError::Overflow)?
        .checked_div(2)
        .ok_or(MathError::DivisionByZero)
}

/// Relative deviation in bps: |a - b| / b * BPS.
pub fn deviation_bps(new_price: i128, old_price: i128) -> Result<i128, MathError> {
    require_positive(new_price)?;
    require_positive(old_price)?;
    let diff = abs_i128(
        new_price
            .checked_sub(old_price)
            .ok_or(MathError::Overflow)?,
    )?;
    mul_div_ceil(diff, BPS, old_price)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PRICE_SCALE;

    #[test]
    fn oracle_scale_is_fourteen_decimals() {
        assert_eq!(PRICE_SCALE, 10_i128.pow(14));
        assert_eq!(to_usd(SCALAR_7, 100_000_000_000_000).unwrap(), SCALAR_7);
    }

    #[test]
    fn to_usd_xlm_at_forty_cents() {
        let amount = 10 * SCALAR_7; // 10 XLM
        let price = 40_000_000_000_000; // $0.40
        assert_eq!(to_usd(amount, price).unwrap(), 4 * SCALAR_7); // $4
    }

    #[test]
    fn from_usd_round_trip_floor() {
        let usd = 4 * SCALAR_7;
        let price = 40_000_000_000_000;
        let tokens = from_usd(usd, price).unwrap();
        assert_eq!(tokens, 10 * SCALAR_7);
        assert_eq!(to_usd(tokens, price).unwrap(), usd);
    }

    #[test]
    fn from_usd_floors_dust() {
        let usd = 1;
        let price = PRICE_SCALE * 3;
        assert_eq!(from_usd(usd, price).unwrap(), 0);
    }

    #[test]
    fn utilization_caps_at_one() {
        assert_eq!(utilization(0, 100).unwrap(), 0);
        assert_eq!(utilization(50, 100).unwrap(), SCALAR_7 / 2);
        assert_eq!(utilization(100, 100).unwrap(), SCALAR_7);
        assert_eq!(utilization(150, 100).unwrap(), SCALAR_7);
        assert_eq!(utilization(1, 0).unwrap(), 0);
    }

    #[test]
    fn min_max_spread() {
        let (min, max) = min_max_from_mid(PRICE_SCALE, 100).unwrap(); // 1%
        assert_eq!(min, PRICE_SCALE * 99 / 100);
        assert_eq!(max, PRICE_SCALE * 101 / 100);
    }

    #[test]
    fn deviation_detects_jump() {
        let old = PRICE_SCALE;
        let new = PRICE_SCALE * 2;
        assert_eq!(deviation_bps(new, old).unwrap(), 10_000);
        assert_eq!(deviation_bps(old, old).unwrap(), 0);
    }

    #[test]
    fn rejects_zero_and_negative() {
        assert_eq!(require_positive(0), Err(MathError::Negative));
        assert_eq!(require_positive(-1), Err(MathError::Negative));
        assert!(to_usd(-1, PRICE_SCALE).is_err());
        assert!(to_usd(1, 0).is_err());
        assert!(mul_div_floor(1, 1, 0).is_err());
    }

    #[test]
    fn bps_rounding_favors_protocol_on_fees() {
        // 1 stroop * 1 bps should ceil to 1 on ceil and 0 on floor
        assert_eq!(bps_of_floor(1, 1).unwrap(), 0);
        assert_eq!(bps_of_ceil(1, 1).unwrap(), 1);
        assert_eq!(bps_of_floor(10_000, 1).unwrap(), 1);
    }

    #[test]
    fn signed_mul_div_rounds_toward_floor_and_ceil() {
        assert_eq!(mul_div_floor(-5, 1, 2).unwrap(), -3);
        assert_eq!(mul_div_ceil(-5, 1, 2).unwrap(), -2);
        assert_eq!(mul_div_floor(5, 1, -2).unwrap(), -3);
        assert_eq!(mul_div_ceil(5, 1, -2).unwrap(), -2);
        assert_eq!(mul_div_floor(-5, 1, -2).unwrap(), 2);
        assert_eq!(mul_div_ceil(-5, 1, -2).unwrap(), 3);
    }

    #[test]
    fn mul_div_reports_intermediate_overflow() {
        assert_eq!(mul_div_floor(i128::MAX, 2, 2), Err(MathError::Overflow));
        assert_eq!(mul_div_ceil(i128::MAX, 2, 2), Err(MathError::Overflow));
    }
}
