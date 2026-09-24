use crate::constants::DEFAULT_ADL_PNL_FACTOR_BPS;
use crate::math::{abs_i128, bps_of_ceil, mul_div_floor, require_positive, MathError};

/// USD PnL. Positive is profit for the trader.
pub fn position_pnl_usd(
    size_usd: i128,
    entry_price: i128,
    current_price: i128,
    is_long: bool,
) -> Result<i128, MathError> {
    require_positive(size_usd)?;
    require_positive(entry_price)?;
    require_positive(current_price)?;
    let diff = if is_long {
        current_price
            .checked_sub(entry_price)
            .ok_or(MathError::Overflow)?
    } else {
        entry_price
            .checked_sub(current_price)
            .ok_or(MathError::Overflow)?
    };
    let sign = if diff < 0 { -1 } else { 1 };
    let mag = mul_div_floor(size_usd, abs_i128(diff)?, entry_price)?;
    mag.checked_mul(sign).ok_or(MathError::Overflow)
}

pub fn leverage_x(size_usd: i128, collateral_usd: i128) -> Result<i128, MathError> {
    require_positive(size_usd)?;
    require_positive(collateral_usd)?;
    mul_div_floor(size_usd, 1, collateral_usd)
}

pub fn exceeds_max_leverage(
    size_usd: i128,
    collateral_usd: i128,
    max_leverage: u32,
) -> Result<bool, MathError> {
    if collateral_usd <= 0 {
        return Ok(true);
    }
    // size / collateral > max  <=>  size > collateral * max
    let cap = collateral_usd
        .checked_mul(i128::from(max_leverage))
        .ok_or(MathError::Overflow)?;
    Ok(size_usd > cap)
}

/// Remaining margin = collateral + pnl - fees. Liquidatable when remaining
/// < size * mmr.
pub fn remaining_margin_usd(
    collateral_usd: i128,
    pnl_usd: i128,
    fees_usd: i128,
) -> Result<i128, MathError> {
    collateral_usd
        .checked_add(pnl_usd)
        .ok_or(MathError::Overflow)?
        .checked_sub(fees_usd)
        .ok_or(MathError::Overflow)
}

pub fn maintenance_required(size_usd: i128, mmr_bps: u32) -> Result<i128, MathError> {
    bps_of_ceil(size_usd, mmr_bps)
}

pub fn is_liquidatable(
    collateral_usd: i128,
    pnl_usd: i128,
    fees_usd: i128,
    size_usd: i128,
    mmr_bps: u32,
) -> Result<bool, MathError> {
    let remaining = remaining_margin_usd(collateral_usd, pnl_usd, fees_usd)?;
    let mm = maintenance_required(size_usd, mmr_bps)?;
    Ok(remaining < mm)
}

/// Approximate liquidation price for display / TP-SL checks.
/// Long: entry * (1 - (collateral - fees - mm) / size)
pub fn liquidation_price(
    size_usd: i128,
    collateral_usd: i128,
    entry_price: i128,
    fees_usd: i128,
    mmr_bps: u32,
    is_long: bool,
) -> Result<i128, MathError> {
    require_positive(size_usd)?;
    require_positive(entry_price)?;
    let mm = maintenance_required(size_usd, mmr_bps)?;
    let buffer = collateral_usd
        .checked_sub(fees_usd)
        .ok_or(MathError::Overflow)?
        .checked_sub(mm)
        .ok_or(MathError::Overflow)?;
    let move_frac = mul_div_floor(buffer, entry_price, size_usd)?;
    if is_long {
        entry_price
            .checked_sub(move_frac)
            .ok_or(MathError::Overflow)
    } else {
        entry_price
            .checked_add(move_frac)
            .ok_or(MathError::Overflow)
    }
}

pub fn should_adl(
    pending_pnl_usd: i128,
    pool_usd: i128,
    factor_bps: u32,
) -> Result<bool, MathError> {
    if pending_pnl_usd <= 0 || pool_usd <= 0 {
        return Ok(false);
    }
    let threshold = crate::math::bps_of_floor(pool_usd, factor_bps)?;
    Ok(pending_pnl_usd >= threshold)
}

pub fn cap_pending_pnl(
    pending_pnl_usd: i128,
    pool_usd: i128,
    factor_bps: u32,
) -> Result<i128, MathError> {
    if pending_pnl_usd <= 0 {
        return Ok(pending_pnl_usd);
    }
    let cap = crate::math::bps_of_floor(pool_usd, factor_bps)?;
    Ok(pending_pnl_usd.min(cap))
}

/// Weighted average entry after adding size.
pub fn blend_entry_price(
    old_size: i128,
    old_entry: i128,
    add_size: i128,
    add_entry: i128,
) -> Result<i128, MathError> {
    require_positive(add_entry)?;
    let sum_size = old_size.checked_add(add_size).ok_or(MathError::Overflow)?;
    require_positive(sum_size)?;
    let a = mul_div_floor(old_size, old_entry, sum_size)?;
    let b = mul_div_floor(add_size, add_entry, sum_size)?;
    a.checked_add(b).ok_or(MathError::Overflow)
}

/// Acceptable-price check. Long increase/short decrease cannot pay more than
/// `acceptable`. Long decrease/short increase cannot receive less than it.
pub fn price_acceptable(
    exec_price: i128,
    acceptable: i128,
    is_long: bool,
    is_increase: bool,
) -> Result<bool, MathError> {
    require_positive(exec_price)?;
    require_positive(acceptable)?;
    let worse_is_higher = is_long == is_increase;
    if worse_is_higher {
        Ok(exec_price <= acceptable)
    } else {
        Ok(exec_price >= acceptable)
    }
}

/// Trigger checks for TP/SL using min/max oracle prices (GMX convention).
pub fn tp_triggered(is_long: bool, min_price: i128, max_price: i128, trigger: i128) -> bool {
    if is_long {
        min_price >= trigger
    } else {
        max_price <= trigger
    }
}

pub fn sl_triggered(is_long: bool, min_price: i128, max_price: i128, trigger: i128) -> bool {
    if is_long {
        min_price <= trigger
    } else {
        max_price >= trigger
    }
}

pub fn limit_increase_triggered(
    is_long: bool,
    min_price: i128,
    max_price: i128,
    trigger: i128,
) -> bool {
    // Long limit: buy at or below trigger (max <= trigger)
    // Short limit: sell at or above trigger (min >= trigger)
    if is_long {
        max_price <= trigger
    } else {
        min_price >= trigger
    }
}

/// Limit decrease / take-profit: long sells at or above trigger; short covers at or below.
pub fn limit_decrease_triggered(
    is_long: bool,
    min_price: i128,
    max_price: i128,
    trigger: i128,
) -> bool {
    if is_long {
        min_price >= trigger
    } else {
        max_price <= trigger
    }
}

/// TWAP slice size. Last slice takes the remainder.
pub fn twap_slice(remaining: i128, parts_left: u32) -> Result<i128, MathError> {
    require_positive(remaining)?;
    if parts_left == 0 {
        return Err(MathError::Invalid);
    }
    if parts_left == 1 {
        return Ok(remaining);
    }
    let slice = remaining / i128::from(parts_left);
    if slice <= 0 {
        Ok(remaining)
    } else {
        Ok(slice)
    }
}

pub fn twap_ready(last_ts: u64, now: u64, interval: u64) -> bool {
    interval == 0 || now.saturating_sub(last_ts) >= interval
}

pub fn default_adl_threshold(pool_usd: i128) -> Result<i128, MathError> {
    crate::math::bps_of_floor(pool_usd, DEFAULT_ADL_PNL_FACTOR_BPS)
}

/// Cross-margin: liquidate the account when combined remaining < combined MM.
pub fn account_is_liquidatable(
    total_collateral_usd: i128,
    total_pnl_usd: i128,
    total_fees_usd: i128,
    total_size_usd: i128,
    mmr_bps: u32,
) -> Result<bool, MathError> {
    is_liquidatable(
        total_collateral_usd,
        total_pnl_usd,
        total_fees_usd,
        total_size_usd,
        mmr_bps,
    )
}

pub fn oi_cap_ok(next_oi: i128, cap: i128) -> bool {
    cap == 0 || next_oi <= cap
}

/// Execution price after impact. Impact USD / notional converted to a price tweak.
pub fn apply_impact_to_price(
    price: i128,
    impact_usd: i128,
    notional_usd: i128,
    is_long: bool,
    is_increase: bool,
) -> Result<i128, MathError> {
    require_positive(price)?;
    require_positive(notional_usd)?;
    if impact_usd == 0 {
        return Ok(price);
    }
    let abs_imp = abs_i128(impact_usd)?;
    let tweak = mul_div_floor(price, abs_imp, notional_usd)?;
    // Positive impact (trader pays) moves price against them.
    let against = impact_usd > 0;
    let higher_is_worse = is_long == is_increase;
    let move_up = if against {
        higher_is_worse
    } else {
        !higher_is_worse
    };
    if move_up {
        price.checked_add(tweak).ok_or(MathError::Overflow)
    } else {
        let p = price.checked_sub(tweak).ok_or(MathError::Overflow)?;
        if p <= 0 {
            Err(MathError::Invalid)
        } else {
            Ok(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{PRICE_SCALE, SCALAR_7};

    #[test]
    fn long_pnl_up() {
        let pnl = position_pnl_usd(10 * SCALAR_7, PRICE_SCALE, PRICE_SCALE * 2, true).unwrap();
        assert_eq!(pnl, 10 * SCALAR_7);
    }

    #[test]
    fn long_pnl_down() {
        let pnl = position_pnl_usd(10 * SCALAR_7, PRICE_SCALE, PRICE_SCALE / 2, true).unwrap();
        assert_eq!(pnl, -5 * SCALAR_7);
    }

    #[test]
    fn short_pnl_down() {
        let pnl = position_pnl_usd(10 * SCALAR_7, PRICE_SCALE, PRICE_SCALE / 2, false).unwrap();
        assert_eq!(pnl, 5 * SCALAR_7);
    }

    #[test]
    fn leverage_and_cap() {
        assert!(!exceeds_max_leverage(20 * SCALAR_7, SCALAR_7, 20).unwrap());
        assert!(exceeds_max_leverage(21 * SCALAR_7, SCALAR_7, 20).unwrap());
        assert!(exceeds_max_leverage(1, 0, 20).unwrap());
    }

    #[test]
    fn liquidation_at_mmr() {
        let size = 20 * SCALAR_7;
        let collat = SCALAR_7;
        let mmr = 500; // 5% of 20 = 1 USD, equal to collateral at entry
                       // at entry pnl=0 fees=0 remaining=1, mm=1, not liquidatable (strict <)
        assert!(!is_liquidatable(collat, 0, 0, size, mmr).unwrap());
        // small adverse pnl
        assert!(is_liquidatable(collat, -1, 0, size, mmr).unwrap());
    }

    #[test]
    fn liq_price_long_below_entry() {
        let p = liquidation_price(20 * SCALAR_7, 2 * SCALAR_7, PRICE_SCALE, 0, 500, true).unwrap();
        assert!(p < PRICE_SCALE);
        assert!(p > 0);
    }

    #[test]
    fn adl_triggers_on_high_pending_pnl() {
        assert!(!should_adl(1, 100, 5_000).unwrap());
        assert!(should_adl(50, 100, 5_000).unwrap());
        assert!(!should_adl(-10, 100, 5_000).unwrap());
    }

    #[test]
    fn pnl_cap_on_deposits() {
        assert_eq!(cap_pending_pnl(100, 100, 8_000).unwrap(), 80);
        assert_eq!(cap_pending_pnl(-20, 100, 8_000).unwrap(), -20);
    }

    #[test]
    fn blend_entry() {
        let p = blend_entry_price(10, 100, 10, 200).unwrap();
        assert_eq!(p, 150);
    }

    #[test]
    fn acceptable_price_long_increase() {
        assert!(price_acceptable(100, 105, true, true).unwrap());
        assert!(!price_acceptable(106, 105, true, true).unwrap());
        assert!(price_acceptable(100, 95, true, false).unwrap());
        assert!(!price_acceptable(94, 95, true, false).unwrap());
    }

    #[test]
    fn tp_sl_triggers() {
        assert!(tp_triggered(true, 110, 112, 110));
        assert!(!tp_triggered(true, 109, 112, 110));
        assert!(sl_triggered(true, 90, 92, 90));
        assert!(limit_increase_triggered(true, 98, 99, 100));
        assert!(!limit_increase_triggered(true, 100, 101, 100));
        assert!(limit_decrease_triggered(true, 110, 112, 110));
        assert!(!limit_decrease_triggered(true, 108, 109, 110));
    }

    #[test]
    fn twap_slices_then_remainder() {
        assert_eq!(twap_slice(100, 4).unwrap(), 25);
        assert_eq!(twap_slice(10, 1).unwrap(), 10);
        assert!(twap_ready(0, 60, 60));
        assert!(!twap_ready(0, 59, 60));
    }

    #[test]
    fn impact_moves_long_open_against() {
        let p = apply_impact_to_price(PRICE_SCALE, SCALAR_7 / 100, SCALAR_7, true, true).unwrap();
        assert!(p > PRICE_SCALE);
    }
}
