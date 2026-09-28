#![no_std]
#![allow(clippy::too_many_arguments)]

use lumix_core::mid_price;
use soroban_sdk::{contractclient, contracttype, Address, Env, Val};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OraclePrice {
    pub min: i128,
    pub max: i128,
    pub timestamp: u64,
}

impl OraclePrice {
    pub fn mid(&self) -> i128 {
        mid_price(self.min, self.max).unwrap_or(self.min)
    }
}

#[contractclient(name = "OracleContractClient")]
pub trait OracleContract {
    fn set_price(env: Env, caller: Address, asset: Address, min: i128, max: i128);
    fn quote(env: Env, asset: Address) -> OraclePrice;
    fn quote_at(env: Env, asset: Address, timestamp: u64) -> OraclePrice;
}

/// Adapter boundary for any future external oracle. Never substitutes a latest
/// observation for a historical request. Return values use USD, 14 decimals.
#[contractclient(name = "PriceSourceClient")]
pub trait PriceSource {
    fn price_at(env: Env, asset: Address, timestamp: u64) -> OraclePrice;
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolStateView {
    pub reserved_long: i128,
    pub reserved_short: i128,
    pub total_shares: i128,
    pub pending_trader_pnl: i128,
}

#[contractclient(name = "PoolContractClient")]
pub trait PoolContract {
    fn set_market(env: Env, market: Address);
    fn deposit(
        env: Env,
        user: Address,
        long_amount: i128,
        short_amount: i128,
        min_shares: i128,
    ) -> i128;
    fn withdraw(
        env: Env,
        user: Address,
        shares: i128,
        min_long: i128,
        min_short: i128,
    ) -> (i128, i128);
    fn adjust_collateral(env: Env, delta: i128);
    fn collateral(env: Env) -> i128;
    fn reserve(env: Env, is_long: bool, amount: i128);
    fn release(env: Env, is_long: bool, amount: i128);
    fn pay(env: Env, to: Address, token: Address, amount: i128);
    fn set_pending_pnl(env: Env, pnl: i128);
    fn get_state(env: Env) -> PoolStateView;
    fn pool_value(env: Env) -> i128;
    fn pool_value_at(env: Env, timestamp: u64) -> i128;
    fn oracle(env: Env) -> Address;
    fn long_token(env: Env) -> Address;
    fn short_token(env: Env) -> Address;
    fn balance(env: Env, id: Address) -> i128;
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketConfigView {
    pub admin: Address,
    pub exchange: Option<Address>,
    pub pool: Address,
    pub oracle: Address,
    pub index_token: Address,
    pub long_token: Address,
    pub short_token: Address,
    pub fee_receiver: Address,
    pub paused: bool,
    pub trading_enabled: bool,
    pub synthetic: bool,
    pub max_leverage: u32,
    pub mmr_bps: u32,
    pub liq_fee_bps: u32,
    pub position_fee_balanced_bps: u32,
    pub position_fee_imbalanced_bps: u32,
    pub protocol_fee_bps: u32,
    pub max_oi_long_usd: i128,
    pub max_oi_short_usd: i128,
    pub max_oi_factor_bps: u32,
    pub impact_factor_bps: u32,
    pub impact_cap_bps: u32,
    pub adl_factor_bps: u32,
    pub off_hours_max_leverage: u32,
    pub off_hours_mmr_bps: u32,
    pub on_hours: bool,
}

#[contractclient(name = "MarketContractClient")]
pub trait MarketContract {
    fn config(env: Env) -> MarketConfigView;
    fn increase(
        env: Env,
        account: Address,
        is_long: bool,
        collateral_amount: i128,
        size_delta_usd: i128,
        acceptable_price: i128,
        isolated: bool,
        timestamp: u64,
    ) -> Val;
    fn decrease(
        env: Env,
        account: Address,
        is_long: bool,
        collateral_withdrawal: i128,
        size_delta_usd: i128,
        acceptable_price: i128,
        receiver: Address,
        timestamp: u64,
    ) -> i128;
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryMarketInfo {
    pub market: Address,
    pub pool: Address,
    pub oracle: Address,
    pub index_token: Address,
    pub long_token: Address,
    pub short_token: Address,
    pub enabled: bool,
}

#[contractclient(name = "FactoryContractClient")]
pub trait FactoryContract {
    fn get_market(env: Env, index_token: Address) -> FactoryMarketInfo;
}
