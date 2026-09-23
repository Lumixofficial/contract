use crate::constants::MINIMUM_LIQUIDITY;
use crate::math::{mul_div_floor, require_positive, MathError};

/// Shares minted for a USD deposit. First depositor is charged MINIMUM_LIQUIDITY
/// (permanently locked) to prevent share-inflation attacks.
pub fn shares_to_mint(
    usd: i128,
    total_shares: i128,
    pool_usd: i128,
) -> Result<(i128, i128), MathError> {
    require_positive(usd)?;
    if total_shares == 0 {
        if usd <= MINIMUM_LIQUIDITY {
            return Err(MathError::Invalid);
        }
        let user = usd
            .checked_sub(MINIMUM_LIQUIDITY)
            .ok_or(MathError::Overflow)?;
        return Ok((user, MINIMUM_LIQUIDITY));
    }
    require_positive(pool_usd)?;
    let minted = mul_div_floor(usd, total_shares, pool_usd)?;
    if minted == 0 {
        return Err(MathError::Invalid);
    }
    Ok((minted, 0))
}

pub fn usd_for_shares(shares: i128, total_shares: i128, pool_usd: i128) -> Result<i128, MathError> {
    require_positive(shares)?;
    require_positive(total_shares)?;
    if shares > total_shares {
        return Err(MathError::Invalid);
    }
    mul_div_floor(shares, pool_usd, total_shares)
}

/// Pro-rata token amounts for a share redemption. Rounds down to the user.
pub fn amounts_for_shares(
    shares: i128,
    total_shares: i128,
    long_balance: i128,
    short_balance: i128,
) -> Result<(i128, i128), MathError> {
    require_positive(shares)?;
    require_positive(total_shares)?;
    if shares > total_shares {
        return Err(MathError::Invalid);
    }
    let long = mul_div_floor(shares, long_balance, total_shares)?;
    let short = mul_div_floor(shares, short_balance, total_shares)?;
    Ok((long, short))
}

/// Available tokens for LP withdrawal after reserving for open interest.
pub fn available_tokens(
    balance: i128,
    reserved: i128,
    reserve_factor_bps: u32,
) -> Result<i128, MathError> {
    let reservable = crate::math::bps_of_floor(balance, reserve_factor_bps)?;
    if reserved >= reservable {
        return Ok(0);
    }
    reservable.checked_sub(reserved).ok_or(MathError::Overflow)
}

pub fn can_reserve(
    balance: i128,
    reserved: i128,
    additional: i128,
    reserve_factor_bps: u32,
) -> Result<bool, MathError> {
    let cap = crate::math::bps_of_floor(balance, reserve_factor_bps)?;
    let next = reserved
        .checked_add(additional)
        .ok_or(MathError::Overflow)?;
    Ok(next <= cap)
}

/// Pool USD used for share pricing. Pending trader PnL is added (capped).
pub fn pool_value_usd(
    long_usd: i128,
    short_usd: i128,
    pending_trader_pnl: i128,
) -> Result<i128, MathError> {
    let assets = long_usd.checked_add(short_usd).ok_or(MathError::Overflow)?;
    // Trader profit reduces pool value.
    let value = assets
        .checked_sub(pending_trader_pnl)
        .ok_or(MathError::Overflow)?;
    if value < 0 {
        Ok(0)
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::SCALAR_7;

    #[test]
    fn first_mint_locks_minimum() {
        let (user, locked) = shares_to_mint(10_000, 0, 0).unwrap();
        assert_eq!(locked, MINIMUM_LIQUIDITY);
        assert_eq!(user, 9_000);
        assert!(shares_to_mint(MINIMUM_LIQUIDITY, 0, 0).is_err());
    }

    #[test]
    fn subsequent_mint_pro_rata() {
        let (m, lock) = shares_to_mint(5_000, 9_000, 9_000).unwrap();
        assert_eq!(lock, 0);
        assert_eq!(m, 5_000);
    }

    #[test]
    fn dust_deposit_rejected() {
        assert!(shares_to_mint(1, 1_000_000, 1_000_000_000).is_err());
    }

    #[test]
    fn redeem_rounds_down() {
        let (l, s) = amounts_for_shares(1, 3, 100, 100).unwrap();
        assert_eq!(l, 33);
        assert_eq!(s, 33);
    }

    #[test]
    fn available_respects_reserve_factor() {
        let a = available_tokens(100, 0, 8_500).unwrap();
        assert_eq!(a, 85);
        let b = available_tokens(100, 85, 8_500).unwrap();
        assert_eq!(b, 0);
        assert!(can_reserve(100, 80, 5, 8_500).unwrap());
        assert!(!can_reserve(100, 80, 6, 8_500).unwrap());
    }

    #[test]
    fn trader_profit_lowers_share_price() {
        let v = pool_value_usd(100, 100, 50).unwrap();
        assert_eq!(v, 150);
        let zero = pool_value_usd(10, 10, 50).unwrap();
        assert_eq!(zero, 0);
    }

    #[test]
    fn usd_for_shares_pro_rata() {
        assert_eq!(usd_for_shares(25, 100, 400).unwrap(), 100);
        assert!(usd_for_shares(101, 100, 400).is_err());
    }

    #[test]
    fn share_price_rises_with_fees() {
        let deposit = 10 * SCALAR_7;
        let (s, lock) = shares_to_mint(deposit, 0, 0).unwrap();
        let total = s + lock;
        let after_fees = deposit + SCALAR_7; // $1 fee
        let usd = usd_for_shares(s, total, after_fees).unwrap();
        assert!(usd > s); // user shares now worth more than original user allocation
    }
}
