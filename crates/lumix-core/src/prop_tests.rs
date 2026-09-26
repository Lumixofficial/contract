use crate::constants::{BPS, PRICE_SCALE, SCALAR_7};
use crate::fees::{
    accrue_borrow, accrue_funding, position_borrow_usd, position_funding_usd, price_impact_usd,
    BorrowState, FundingState,
};
use crate::math::{mul_div_floor, to_usd};
use crate::position::{
    cap_pending_pnl, exceeds_max_leverage, is_liquidatable, position_pnl_usd, should_adl,
};
use crate::shares::{pool_value_usd, shares_to_mint, usd_for_shares};
use crate::{option_buyer_payout, option_writer_collateral};
use proptest::prelude::*;

proptest! {
    #[test]
    fn to_usd_never_overflows_reasonable_inputs(
        amount in 0i128..=1_000_000_000_000_000,
        price in 1i128..=PRICE_SCALE * 1_000_000
    ) {
        let _ = to_usd(amount, price);
    }

    #[test]
    fn mint_then_redeem_does_not_profit(
        deposit in (crate::MINIMUM_LIQUIDITY + 1)..=10_000_000_000i128
    ) {
        let (user, lock) = shares_to_mint(deposit, 0, 0).unwrap();
        let total = user + lock;
        let out = usd_for_shares(user, total, deposit).unwrap();
        prop_assert!(out <= deposit);
        prop_assert!(out + lock <= deposit);
    }

    #[test]
    fn leverage_cap_matches_definition(
        size in 1i128..=1_000_000_000,
        collat in 1i128..=1_000_000_000,
        max in 1u32..=100
    ) {
        let exceeds = exceeds_max_leverage(size, collat, max).unwrap();
        prop_assert_eq!(exceeds, size > collat * i128::from(max));
    }

    #[test]
    fn long_pnl_zero_at_entry(
        size in 1i128..=1_000_000_000,
        price in 1i128..=PRICE_SCALE * 1000
    ) {
        let pnl = position_pnl_usd(size, price, price, true).unwrap();
        prop_assert_eq!(pnl, 0);
        let pnl_s = position_pnl_usd(size, price, price, false).unwrap();
        prop_assert_eq!(pnl_s, 0);
    }

    #[test]
    fn impact_zero_when_balance_unchanged(
        long in 0i128..=1_000_000,
        short in 0i128..=1_000_000,
        notional in 1i128..=1_000_000
    ) {
        let i = price_impact_usd(long, short, long, short, notional, 10, 50).unwrap();
        prop_assert_eq!(i, 0);
    }

    #[test]
    fn funding_empty_book_is_noop(dt in 1u64..=1_000_000) {
        let s = FundingState::new(0);
        let n = accrue_funding(s, 0, 0, dt).unwrap();
        prop_assert_eq!(n.cumulative_long, 0);
        prop_assert_eq!(n.cumulative_short, 0);
    }

    #[test]
    fn pool_value_never_negative(
        long in 0i128..=1_000_000_000,
        short in 0i128..=1_000_000_000,
        pnl in -1_000_000_000i128..=1_000_000_000
    ) {
        let v = pool_value_usd(long, short, pnl).unwrap();
        prop_assert!(v >= 0);
        let capped = cap_pending_pnl(pnl.max(0), v.max(1), 8_000).unwrap();
        prop_assert!(capped >= 0);
    }

    #[test]
    fn liquidatable_implies_margin_below_mm(
        collat in 0i128..=10_000_000,
        pnl in -10_000_000i128..=10_000_000,
        fees in 0i128..=1_000_000,
        size in 1i128..=10_000_000,
        mmr in 1u32..=2_000
    ) {
        let liq = is_liquidatable(collat, pnl, fees, size, mmr).unwrap();
        let remaining = collat + pnl - fees;
        let mm = crate::math::bps_of_ceil(size, mmr).unwrap();
        prop_assert_eq!(liq, remaining < mm);
    }

    #[test]
    fn adl_only_on_profit(
        pnl in -1_000_000i128..=1_000_000,
        pool in 1i128..=1_000_000
    ) {
        let adl = should_adl(pnl, pool, 5_000).unwrap();
        if pnl <= 0 {
            prop_assert!(!adl);
        }
    }

    #[test]
    fn bps_of_never_exceeds_amount(amount in 0i128..=i128::MAX / BPS, bps in 0u32..=10_000) {
        let f = crate::math::bps_of_floor(amount, bps).unwrap();
        let c = crate::math::bps_of_ceil(amount, bps).unwrap();
        prop_assert!(f <= amount);
        prop_assert!(c <= amount || bps == 10_000 || amount == 0);
        prop_assert!(f <= c);
    }

    #[test]
    fn option_payout_never_exceeds_collateral(
        amount in 1i128..=1_000_000_000_000_000,
        strike in 1i128..=PRICE_SCALE * 1_000_000,
        spot in 1i128..=PRICE_SCALE * 1_000_000,
        is_call in any::<bool>(),
    ) {
        let collateral = option_writer_collateral(amount, strike, is_call).unwrap();
        let payout = option_buyer_payout(amount, strike, spot, is_call).unwrap();
        prop_assert!(payout >= 0);
        prop_assert!(payout <= collateral);
    }
}

#[test]
fn borrow_and_funding_settle_independently() {
    let mut b = BorrowState::new(0);
    let mut f = FundingState::new(0);
    b = accrue_borrow(
        b,
        70 * SCALAR_7,
        30 * SCALAR_7,
        50 * SCALAR_7,
        100 * SCALAR_7,
        3600,
    )
    .unwrap();
    f = accrue_funding(f, 70 * SCALAR_7, 30 * SCALAR_7, 3600).unwrap();
    let borrow = position_borrow_usd(10 * SCALAR_7, true, 0, &b).unwrap();
    let fund = position_funding_usd(10 * SCALAR_7, true, 0, &f).unwrap();
    assert!(borrow > 0);
    assert!(fund > 0);
}

#[test]
fn mul_div_matches_naive_when_no_overflow() {
    let r = mul_div_floor(100, 200, 50).unwrap();
    assert_eq!(r, 400);
}
