//! Protocol-wide scales.
//!
//! Token amounts use 7 decimals (Stellar stroops).
//! Oracle prices use 14 decimals (Reflector).
//! USD notionals are 7 decimals (`amount * price / PRICE_SCALE`).
//! Rates use 7 decimals (`1.0 = SCALAR_7`).

/// 1.0 with 7 decimals.
pub const SCALAR_7: i128 = 10_000_000;
/// 1.0 with 12 decimals (cumulative funding / borrow indices).
pub const SCALAR_12: i128 = 1_000_000_000_000;
/// Reflector / Lumix oracle price scale.
pub const PRICE_SCALE: i128 = 100_000_000_000_000;
/// Basis points. 1 bps = 0.01%.
pub const BPS: i128 = 10_000;
pub const BPS_U32: u32 = 10_000;

pub const SECONDS_PER_HOUR: i128 = 3_600;
pub const SECONDS_PER_YEAR: i128 = 31_536_000;

/// Uniswap-style dead shares burned on the first mint to block inflation attacks.
pub const MINIMUM_LIQUIDITY: i128 = 1_000;

/// Instance storage TTL (~30 days at 5s ledgers).
pub const INSTANCE_TTL_THRESHOLD: u32 = 120_960;
pub const INSTANCE_TTL_EXTEND_TO: u32 = 518_400;
/// Persistent storage TTL.
pub const PERSISTENT_TTL_THRESHOLD: u32 = 120_960;
pub const PERSISTENT_TTL_EXTEND_TO: u32 = 518_400;

/// Default v1 risk parameters for an XLM/USD fully-backed market.
pub const DEFAULT_MAX_LEVERAGE: u32 = 20;
pub const DEFAULT_MMR_BPS: u32 = 500;
pub const DEFAULT_LIQ_FEE_BPS: u32 = 100;
pub const DEFAULT_RESERVE_FACTOR_BPS: u32 = 8_500;
pub const DEFAULT_POSITION_FEE_BALANCED_BPS: u32 = 5;
pub const DEFAULT_POSITION_FEE_IMBALANCED_BPS: u32 = 7;
pub const DEFAULT_SWAP_FEE_BALANCED_BPS: u32 = 5;
pub const DEFAULT_SWAP_FEE_IMBALANCED_BPS: u32 = 7;
pub const DEFAULT_ATOMIC_SWAP_FEE_BPS: u32 = 375;
pub const DEFAULT_PROTOCOL_FEE_BPS: u32 = 2_000;
pub const DEFAULT_IMPACT_FACTOR_BPS: u32 = 10;
pub const DEFAULT_IMPACT_CAP_BPS: u32 = 50;
pub const DEFAULT_MAX_PNL_FACTOR_BPS: u32 = 8_000;
pub const DEFAULT_ADL_PNL_FACTOR_BPS: u32 = 5_000;
pub const DEFAULT_MAX_OI_FACTOR_BPS: u32 = 8_000;
pub const DEFAULT_ORACLE_MAX_AGE: u64 = 600;
pub const DEFAULT_ORACLE_MAX_DEVIATION_BPS: u32 = 2_000;
pub const DEFAULT_ORACLE_SPREAD_BPS: u32 = 2;
pub const DEFAULT_FUNDING_INCREASE_PER_SECOND: i128 = 15; // ~47% APR at full imbalance
pub const DEFAULT_FUNDING_DECREASE_PER_SECOND: i128 = 8;
pub const DEFAULT_MIN_FUNDING_PER_SECOND: i128 = 1;
pub const DEFAULT_MAX_FUNDING_PER_SECOND: i128 = 95; // ~300% APR cap
pub const DEFAULT_FUNDING_STABLE_THRESHOLD: i128 = 2_000_000; // 0.20
pub const DEFAULT_FUNDING_DECREASE_THRESHOLD: i128 = 500_000; // 0.05
pub const DEFAULT_BORROW_KINK: i128 = 7_500_000; // 75%
pub const DEFAULT_BORROW_SLOPE1_APR: i128 = 4_500_000; // 45%
pub const DEFAULT_BORROW_SLOPE2_APR: i128 = 10_000_000; // 100%
pub const DEFAULT_REBATE_DELAY: u64 = 5 * 86_400;
pub const DEFAULT_MAX_SLIPPAGE_BPS: u32 = 500;
