use crate::constants::{
    BPS, DEFAULT_BORROW_KINK, DEFAULT_BORROW_SLOPE1_APR, DEFAULT_BORROW_SLOPE2_APR,
    DEFAULT_FUNDING_DECREASE_PER_SECOND, DEFAULT_FUNDING_DECREASE_THRESHOLD,
    DEFAULT_FUNDING_INCREASE_PER_SECOND, DEFAULT_FUNDING_STABLE_THRESHOLD,
    DEFAULT_MAX_FUNDING_PER_SECOND, DEFAULT_MIN_FUNDING_PER_SECOND, SCALAR_12, SCALAR_7,
    SECONDS_PER_YEAR,
};
use crate::math::{abs_i128, mul_div_ceil, mul_div_floor, MathError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FundingState {
    /// Signed funding factor per second in SCALAR_12. Positive => longs pay shorts.
    pub saved_factor_per_second: i128,
    pub last_timestamp: u64,
    /// Cumulative funding amount per USD size, SCALAR_12. Longs pay when this rises.
    pub cumulative_long: i128,
    pub cumulative_short: i128,
}

impl FundingState {
    pub fn new(now: u64) -> Self {
        Self {
            saved_factor_per_second: 0,
            last_timestamp: now,
            cumulative_long: 0,
            cumulative_short: 0,
        }
    }
}

/// GMX-style adaptive funding. Rate ramps while imbalance stays high, decays
/// toward zero when the book is balanced, and flips immediately on a side change.
pub fn next_funding_factor(
    current: i128,
    long_oi: i128,
    short_oi: i128,
    dt: i128,
) -> Result<i128, MathError> {
    if dt <= 0 {
        return Ok(current);
    }
    let total = long_oi.checked_add(short_oi).ok_or(MathError::Overflow)?;
    if total == 0 || long_oi == 0 || short_oi == 0 {
        return Ok(current);
    }
    let diff = abs_i128(long_oi.checked_sub(short_oi).ok_or(MathError::Overflow)?)?;
    let imbalance = mul_div_floor(diff, SCALAR_7, total)?;
    let longs_pay = long_oi >= short_oi;
    let target_sign: i128 = if longs_pay { 1 } else { -1 };

    let mut factor = current;
    if factor != 0 && factor.signum() != target_sign {
        // Imbalance reversed: start ramping the other way immediately.
        factor = 0;
    }

    let abs_factor = abs_i128(factor)?;
    let next_abs = if imbalance > DEFAULT_FUNDING_STABLE_THRESHOLD {
        let bump = mul_div_ceil(
            imbalance,
            DEFAULT_FUNDING_INCREASE_PER_SECOND * dt,
            SCALAR_7,
        )?;
        let bumped = abs_factor.checked_add(bump).ok_or(MathError::Overflow)?;
        bumped.min(DEFAULT_MAX_FUNDING_PER_SECOND)
    } else if imbalance < DEFAULT_FUNDING_DECREASE_THRESHOLD {
        let down = DEFAULT_FUNDING_DECREASE_PER_SECOND
            .checked_mul(dt)
            .ok_or(MathError::Overflow)?;
        abs_factor
            .saturating_sub(down)
            .max(DEFAULT_MIN_FUNDING_PER_SECOND)
    } else {
        abs_factor.max(DEFAULT_MIN_FUNDING_PER_SECOND)
    };

    Ok(next_abs * target_sign)
}

pub fn accrue_funding(
    mut state: FundingState,
    long_oi: i128,
    short_oi: i128,
    now: u64,
) -> Result<FundingState, MathError> {
    if now < state.last_timestamp {
        return Err(MathError::Invalid);
    }
    let dt = i128::from(now.saturating_sub(state.last_timestamp));
    if dt == 0 {
        return Ok(state);
    }
    let factor = next_funding_factor(state.saved_factor_per_second, long_oi, short_oi, dt)?;
    state.saved_factor_per_second = factor;

    let total = long_oi.checked_add(short_oi).ok_or(MathError::Overflow)?;
    if total > 0 && long_oi > 0 && short_oi > 0 {
        let paying = if factor >= 0 { long_oi } else { short_oi };
        let receiving = if factor >= 0 { short_oi } else { long_oi };
        let abs_f = abs_i128(factor)?;
        // paying_side_index += abs_f * dt
        let pay_delta = abs_f.checked_mul(dt).ok_or(MathError::Overflow)?;
        // receiving gets paying_oi/receiving_oi of that so inflows = outflows
        let recv_delta = mul_div_floor(pay_delta, paying, receiving)?;
        if factor >= 0 {
            state.cumulative_long = state
                .cumulative_long
                .checked_add(pay_delta)
                .ok_or(MathError::Overflow)?;
            state.cumulative_short = state
                .cumulative_short
                .checked_sub(recv_delta)
                .ok_or(MathError::Overflow)?;
        } else {
            state.cumulative_short = state
                .cumulative_short
                .checked_add(pay_delta)
                .ok_or(MathError::Overflow)?;
            state.cumulative_long = state
                .cumulative_long
                .checked_sub(recv_delta)
                .ok_or(MathError::Overflow)?;
        }
    }
    state.last_timestamp = now;
    Ok(state)
}

/// Funding owed by a position in USD (positive = position pays).
pub fn position_funding_usd(
    size_usd: i128,
    is_long: bool,
    checkpoint: i128,
    state: &FundingState,
) -> Result<i128, MathError> {
    let latest = if is_long {
        state.cumulative_long
    } else {
        state.cumulative_short
    };
    let delta = latest.checked_sub(checkpoint).ok_or(MathError::Overflow)?;
    // size * delta / SCALAR_12. Positive delta means this side paid.
    mul_div_ceil(size_usd, delta, SCALAR_12)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowState {
    pub cumulative_long: i128,
    pub cumulative_short: i128,
    pub last_timestamp: u64,
}

impl BorrowState {
    pub fn new(now: u64) -> Self {
        Self {
            cumulative_long: 0,
            cumulative_short: 0,
            last_timestamp: now,
        }
    }
}

/// Kink borrow APR (SCALAR_7) given utilization (SCALAR_7). Only the heavier
/// OI side pays this rate.
pub fn borrow_apr(util: i128) -> Result<i128, MathError> {
    if util <= 0 {
        return Ok(0);
    }
    let util = util.min(SCALAR_7);
    if util <= DEFAULT_BORROW_KINK {
        mul_div_ceil(util, DEFAULT_BORROW_SLOPE1_APR, DEFAULT_BORROW_KINK)
    } else {
        let extra = util
            .checked_sub(DEFAULT_BORROW_KINK)
            .ok_or(MathError::Overflow)?;
        let span = SCALAR_7
            .checked_sub(DEFAULT_BORROW_KINK)
            .ok_or(MathError::Overflow)?;
        let add = mul_div_ceil(
            extra,
            DEFAULT_BORROW_SLOPE2_APR
                .checked_sub(DEFAULT_BORROW_SLOPE1_APR)
                .ok_or(MathError::Overflow)?,
            span,
        )?;
        DEFAULT_BORROW_SLOPE1_APR
            .checked_add(add)
            .ok_or(MathError::Overflow)
    }
}

pub fn borrow_factor_per_second(util: i128) -> Result<i128, MathError> {
    let apr = borrow_apr(util)?;
    // per-second in SCALAR_12: apr/YEAR * SCALAR_12 / SCALAR_7
    mul_div_ceil(
        apr,
        SCALAR_12,
        SCALAR_7
            .checked_mul(SECONDS_PER_YEAR)
            .ok_or(MathError::Overflow)?,
    )
}

pub fn accrue_borrow(
    mut state: BorrowState,
    long_oi: i128,
    short_oi: i128,
    reserved: i128,
    pool_usd: i128,
    now: u64,
) -> Result<BorrowState, MathError> {
    if now < state.last_timestamp {
        return Err(MathError::Invalid);
    }
    let dt = i128::from(now.saturating_sub(state.last_timestamp));
    if dt == 0 {
        return Ok(state);
    }
    let util = crate::math::utilization(reserved, pool_usd)?;
    let fps = borrow_factor_per_second(util)?;
    let delta = fps.checked_mul(dt).ok_or(MathError::Overflow)?;
    if long_oi > short_oi {
        state.cumulative_long = state
            .cumulative_long
            .checked_add(delta)
            .ok_or(MathError::Overflow)?;
    } else if short_oi > long_oi {
        state.cumulative_short = state
            .cumulative_short
            .checked_add(delta)
            .ok_or(MathError::Overflow)?;
    }
    state.last_timestamp = now;
    Ok(state)
}

pub fn position_borrow_usd(
    size_usd: i128,
    is_long: bool,
    checkpoint: i128,
    state: &BorrowState,
) -> Result<i128, MathError> {
    let latest = if is_long {
        state.cumulative_long
    } else {
        state.cumulative_short
    };
    let delta = latest.checked_sub(checkpoint).ok_or(MathError::Overflow)?;
    mul_div_ceil(size_usd, delta, SCALAR_12)
}

/// Balance-aware open/close fee in USD.
pub fn position_fee_usd(
    size_delta_usd: i128,
    long_oi: i128,
    short_oi: i128,
    is_long: bool,
    balanced_bps: u32,
    imbalanced_bps: u32,
) -> Result<(i128, bool), MathError> {
    let improves = if is_long {
        long_oi < short_oi
    } else {
        short_oi < long_oi
    };
    let bps = if improves {
        balanced_bps
    } else {
        imbalanced_bps
    };
    let fee = crate::math::bps_of_ceil(size_delta_usd, bps)?;
    Ok((fee, improves))
}

/// Linear price impact in USD, capped. Positive = trader pays (worse fill).
/// `factor_bps` scales impact with the change in |long - short| relative to total OI.
pub fn price_impact_usd(
    initial_long: i128,
    initial_short: i128,
    next_long: i128,
    next_short: i128,
    notional: i128,
    factor_bps: u32,
    cap_bps: u32,
) -> Result<i128, MathError> {
    let initial_diff = abs_i128(
        initial_long
            .checked_sub(initial_short)
            .ok_or(MathError::Overflow)?,
    )?;
    let next_diff = abs_i128(
        next_long
            .checked_sub(next_short)
            .ok_or(MathError::Overflow)?,
    )?;
    let improved = next_diff < initial_diff;
    let delta = abs_i128(
        next_diff
            .checked_sub(initial_diff)
            .ok_or(MathError::Overflow)?,
    )?;
    let total = next_long
        .checked_add(next_short)
        .ok_or(MathError::Overflow)?
        .max(
            initial_long
                .checked_add(initial_short)
                .ok_or(MathError::Overflow)?,
        )
        .max(1);
    let uncapped = mul_div_ceil(
        notional,
        delta
            .checked_mul(i128::from(factor_bps))
            .ok_or(MathError::Overflow)?,
        total.checked_mul(BPS).ok_or(MathError::Overflow)?,
    )?;
    let cap = crate::math::bps_of_ceil(notional, cap_bps)?;
    let mag = uncapped.min(cap);
    Ok(if improved { -mag } else { mag })
}

/// Same as `price_impact_usd`, plus the USD excess above the cap (claimable rebate).
/// Excess is zero when the trade improves balance or impact is inside the cap.
pub fn price_impact_with_rebate(
    initial_long: i128,
    initial_short: i128,
    next_long: i128,
    next_short: i128,
    notional: i128,
    factor_bps: u32,
    cap_bps: u32,
) -> Result<(i128, i128), MathError> {
    let initial_diff = abs_i128(
        initial_long
            .checked_sub(initial_short)
            .ok_or(MathError::Overflow)?,
    )?;
    let next_diff = abs_i128(
        next_long
            .checked_sub(next_short)
            .ok_or(MathError::Overflow)?,
    )?;
    let improved = next_diff < initial_diff;
    let delta = abs_i128(
        next_diff
            .checked_sub(initial_diff)
            .ok_or(MathError::Overflow)?,
    )?;
    let total = next_long
        .checked_add(next_short)
        .ok_or(MathError::Overflow)?
        .max(
            initial_long
                .checked_add(initial_short)
                .ok_or(MathError::Overflow)?,
        )
        .max(1);
    let uncapped = mul_div_ceil(
        notional,
        delta
            .checked_mul(i128::from(factor_bps))
            .ok_or(MathError::Overflow)?,
        total.checked_mul(BPS).ok_or(MathError::Overflow)?,
    )?;
    let cap = crate::math::bps_of_ceil(notional, cap_bps)?;
    let mag = uncapped.min(cap);
    let applied = if improved { -mag } else { mag };
    let excess = if improved || uncapped <= cap {
        0
    } else {
        uncapped.checked_sub(cap).ok_or(MathError::Overflow)?
    };
    Ok((applied, excess))
}

/// Trader fee after a referral discount. Returns (fee_after_discount, discount).
pub fn apply_trader_discount(fee: i128, discount_bps: u32) -> Result<(i128, i128), MathError> {
    if fee <= 0 {
        return Ok((fee, 0));
    }
    let discount = crate::math::bps_of_floor(fee, discount_bps)?;
    Ok((
        fee.checked_sub(discount).ok_or(MathError::Overflow)?,
        discount,
    ))
}

pub fn split_protocol_fee(fee: i128, protocol_bps: u32) -> Result<(i128, i128), MathError> {
    let protocol = crate::math::bps_of_floor(fee, protocol_bps)?;
    let pool = fee.checked_sub(protocol).ok_or(MathError::Overflow)?;
    Ok((pool, protocol))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::SECONDS_PER_HOUR;

    #[test]
    fn funding_longs_pay_when_long_heavy() {
        let mut s = FundingState::new(0);
        s = accrue_funding(s, 80, 20, 3600).unwrap();
        assert!(s.saved_factor_per_second > 0);
        assert!(s.cumulative_long > 0);
        assert!(s.cumulative_short < 0);
        let long_pays = position_funding_usd(SCALAR_7, true, 0, &s).unwrap();
        let short_recv = position_funding_usd(SCALAR_7, false, 0, &s).unwrap();
        assert!(long_pays > 0);
        assert!(short_recv < 0);
    }

    #[test]
    fn funding_no_accrual_if_one_side_empty() {
        let mut s = FundingState::new(0);
        s = accrue_funding(s, 100, 0, 3600).unwrap();
        assert_eq!(s.cumulative_long, 0);
        assert_eq!(s.cumulative_short, 0);
    }

    #[test]
    fn funding_flips_on_imbalance_reversal() {
        let mut s = FundingState::new(0);
        s = accrue_funding(s, 80, 20, 10_000).unwrap();
        assert!(s.saved_factor_per_second > 0);
        s = accrue_funding(s, 20, 80, s.last_timestamp + 10_000).unwrap();
        assert!(s.saved_factor_per_second < 0);
    }

    #[test]
    fn borrow_only_heavier_side() {
        let mut s = BorrowState::new(0);
        s = accrue_borrow(s, 80, 20, 50, 100, 3600).unwrap();
        assert!(s.cumulative_long > 0);
        assert_eq!(s.cumulative_short, 0);
        let fee = position_borrow_usd(SCALAR_7, true, 0, &s).unwrap();
        assert!(fee > 0);
        assert_eq!(position_borrow_usd(SCALAR_7, false, 0, &s).unwrap(), 0);
    }

    #[test]
    fn borrow_kink_is_monotonic() {
        let a = borrow_apr(SCALAR_7 / 2).unwrap();
        let b = borrow_apr(DEFAULT_BORROW_KINK).unwrap();
        let c = borrow_apr(SCALAR_7).unwrap();
        assert!(a < b && b < c);
        assert_eq!(b, DEFAULT_BORROW_SLOPE1_APR);
        assert_eq!(c, DEFAULT_BORROW_SLOPE2_APR);
    }

    #[test]
    fn position_fee_cheaper_when_balancing() {
        let (bal, improves) = position_fee_usd(SCALAR_7, 20, 80, true, 5, 7).unwrap();
        let (imb, worsens) = position_fee_usd(SCALAR_7, 80, 20, true, 5, 7).unwrap();
        assert!(improves);
        assert!(!worsens);
        assert!(bal < imb);
    }

    #[test]
    fn impact_positive_when_worsening() {
        let pay = price_impact_usd(50, 50, 80, 50, 30, 10, 50).unwrap();
        assert!(pay > 0);
        let rebate = price_impact_usd(80, 50, 50, 50, 30, 10, 50).unwrap();
        assert!(rebate < 0);
    }

    #[test]
    fn impact_is_capped() {
        let huge = price_impact_usd(0, 0, 1_000_000, 0, SCALAR_7, 10_000, 50).unwrap();
        let cap = crate::math::bps_of_ceil(SCALAR_7, 50).unwrap();
        assert_eq!(huge, cap);
    }

    #[test]
    fn protocol_split_keeps_majority_in_pool() {
        let (pool, proto) = split_protocol_fee(10_000, 2_000).unwrap();
        assert_eq!(pool, 8_000);
        assert_eq!(proto, 2_000);
    }

    #[test]
    fn impact_rebate_is_excess_over_cap() {
        let (applied, excess) =
            price_impact_with_rebate(0, 0, 1_000_000, 0, SCALAR_7, 10_000, 50).unwrap();
        let cap = crate::math::bps_of_ceil(SCALAR_7, 50).unwrap();
        assert_eq!(applied, cap);
        assert!(excess > 0);
    }

    #[test]
    fn trader_discount_cuts_fee() {
        let (after, disc) = apply_trader_discount(10_000, 1_000).unwrap();
        assert_eq!(after, 9_000);
        assert_eq!(disc, 1_000);
    }

    #[test]
    fn hourly_borrow_is_small_at_low_util() {
        let fps = borrow_factor_per_second(SCALAR_7 / 10).unwrap();
        let hourly = fps * SECONDS_PER_HOUR;
        // far below 1% of SCALAR_12
        assert!(hourly < SCALAR_12 / 100);
    }
}
