#![no_std]

use lumix_core::{
    limit_decrease_triggered, limit_increase_triggered, sl_triggered, tp_triggered, twap_slice,
    INSTANCE_TTL_EXTEND_TO, INSTANCE_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO,
    PERSISTENT_TTL_THRESHOLD,
};
use lumix_interfaces::{
    FactoryContractClient, MarketConfigView, MarketContractClient, OracleContractClient,
};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, token,
    Address, Env,
};

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn bump_key(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    InvalidAmount = 2,
    NotReady = 3,
    Frozen = 4,
    Missing = 5,
    Cancelled = 6,
    InvalidMarket = 7,
    PendingOrder = 8,
    Executed = 9,
}

pub const MARKET_INCREASE: u32 = 0;
pub const MARKET_DECREASE: u32 = 1;
pub const LIMIT_INCREASE: u32 = 2;
pub const LIMIT_DECREASE: u32 = 3;
pub const TAKE_PROFIT: u32 = 4;
pub const STOP_LOSS: u32 = 5;
pub const TWAP_INCREASE: u32 = 6;
pub const TWAP_DECREASE: u32 = 7;

#[contracttype]
#[derive(Clone, Debug)]
pub struct Order {
    pub id: u32,
    pub created_at: u64,
    pub price_timestamp: u64,
    pub executed: bool,
    pub account: Address,
    pub market: Address,
    pub order_type: u32,
    pub is_long: bool,
    pub collateral_amount: i128,
    pub collateral_token: Address,
    pub size_delta_usd: i128,
    pub acceptable_price: i128,
    pub trigger_price: i128,
    pub execution_fee: i128,
    pub isolated: bool,
    pub frozen: bool,
    pub cancelled: bool,
    pub twap_interval: u64,
    pub twap_parts: u32,
    pub last_slice_ts: u64,
    pub remaining_usd: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Config {
    pub admin: Address,
    pub oracle: Address,
    pub fee_token: Address,
    pub factory: Option<Address>,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    NextId,
    Order(u32),
    Keeper(Address),
    Pending(Address, Address, bool),
}

#[contractevent]
pub struct OrderCreated {
    pub id: u32,
    pub timestamp: u64,
    pub market: Address,
    pub account: Address,
    pub order_type: u32,
}

#[contractevent]
pub struct OrderExecuted {
    pub id: u32,
    pub timestamp: u64,
    pub keeper: Address,
    pub size_usd: i128,
}

#[contract]
pub struct Exchange;

fn cfg(env: &Env) -> Config {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized))
}

fn save_cfg(env: &Env, c: &Config) {
    env.storage().instance().set(&DataKey::Config, c);
}

fn require_admin(c: &Config) {
    c.admin.require_auth();
}

fn is_increase(ty: u32) -> bool {
    ty == MARKET_INCREASE || ty == LIMIT_INCREASE || ty == TWAP_INCREASE
}

fn is_twap(ty: u32) -> bool {
    ty == TWAP_INCREASE || ty == TWAP_DECREASE
}

fn triggered(ty: u32, is_long: bool, min: i128, max: i128, trigger: i128) -> bool {
    match ty {
        MARKET_INCREASE | MARKET_DECREASE => true,
        LIMIT_INCREASE => limit_increase_triggered(is_long, min, max, trigger),
        LIMIT_DECREASE | TAKE_PROFIT => {
            limit_decrease_triggered(is_long, min, max, trigger)
                || tp_triggered(is_long, min, max, trigger)
        }
        STOP_LOSS => sl_triggered(is_long, min, max, trigger),
        TWAP_INCREASE | TWAP_DECREASE => true,
        _ => false,
    }
}

fn load_order(env: &Env, id: u32) -> Order {
    if env.storage().persistent().has(&DataKey::Order(id)) {
        bump_key(env, &DataKey::Order(id));
    }
    env.storage()
        .persistent()
        .get(&DataKey::Order(id))
        .unwrap_or_else(|| panic_with_error!(env, Error::Missing))
}

fn save_order(env: &Env, o: &Order) {
    let key = DataKey::Order(o.id);
    env.storage().persistent().set(&key, o);
    bump_key(env, &key);
    let pending = DataKey::Pending(o.account.clone(), o.market.clone(), o.is_long);
    if env.storage().persistent().has(&pending) {
        bump_key(env, &pending);
    }
}

fn validated_market(env: &Env, c: &Config, market: &Address, increase: bool) -> MarketConfigView {
    let factory = c
        .factory
        .clone()
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidMarket));
    let mcfg = MarketContractClient::new(env, market).config();
    let info = FactoryContractClient::new(env, &factory).get_market(&mcfg.index_token);
    if info.market != *market {
        panic_with_error!(env, Error::InvalidMarket);
    }
    if info.pool != mcfg.pool
        || info.oracle != c.oracle
        || mcfg.oracle != c.oracle
        || info.long_token != mcfg.long_token
        || info.short_token != mcfg.short_token
        || mcfg.exchange != Some(env.current_contract_address())
        || (increase && !info.enabled)
    {
        panic_with_error!(env, Error::InvalidMarket);
    }
    mcfg
}

#[contractimpl]
impl Exchange {
    pub fn __constructor(env: Env, admin: Address, oracle: Address, fee_token: Address) {
        save_cfg(
            &env,
            &Config {
                admin,
                oracle,
                fee_token,
                factory: None,
            },
        );
        env.storage().instance().set(&DataKey::NextId, &0u32);
        bump(&env);
    }

    /// Bind this exchange to one canonical registry before accepting requests.
    pub fn set_factory(env: Env, factory: Address) {
        let mut c = cfg(&env);
        require_admin(&c);
        if c.factory
            .as_ref()
            .is_some_and(|existing| *existing != factory)
            || factory == env.current_contract_address()
        {
            panic_with_error!(&env, Error::InvalidMarket);
        }
        c.factory = Some(factory);
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn set_keeper(env: Env, keeper: Address, allowed: bool) {
        let c = cfg(&env);
        require_admin(&c);
        env.storage()
            .instance()
            .set(&DataKey::Keeper(keeper), &allowed);
        bump(&env);
    }

    pub fn create_order(
        env: Env,
        account: Address,
        market: Address,
        order_type: u32,
        is_long: bool,
        collateral_amount: i128,
        size_delta_usd: i128,
        acceptable_price: i128,
        trigger_price: i128,
        execution_fee: i128,
        isolated: bool,
        twap_interval: u64,
        twap_parts: u32,
    ) -> u32 {
        let c = cfg(&env);
        account.require_auth();
        if order_type > TWAP_DECREASE
            || size_delta_usd < 0
            || collateral_amount < 0
            || execution_fee < 0
            || acceptable_price <= 0
            || trigger_price < 0
            || (size_delta_usd == 0 && collateral_amount == 0)
            || !isolated
            || (is_twap(order_type) && (twap_parts == 0 || size_delta_usd < i128::from(twap_parts)))
        {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        let market_config = validated_market(&env, &c, &market, is_increase(order_type));
        let pending_key = DataKey::Pending(account.clone(), market.clone(), is_long);
        if env.storage().persistent().has(&pending_key) {
            panic_with_error!(&env, Error::PendingOrder);
        }
        if is_increase(order_type) && collateral_amount > 0 {
            token::TokenClient::new(&env, &market_config.short_token).transfer(
                &account,
                env.current_contract_address(),
                &collateral_amount,
            );
        }
        if execution_fee > 0 {
            token::TokenClient::new(&env, &c.fee_token).transfer(
                &account,
                env.current_contract_address(),
                &execution_fee,
            );
        }
        let mut id: u32 = env.storage().instance().get(&DataKey::NextId).unwrap_or(0);
        id = id
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(&env, Error::InvalidAmount));
        env.storage().instance().set(&DataKey::NextId, &id);
        let parts = if is_twap(order_type) {
            twap_parts.max(1)
        } else {
            1
        };
        let o = Order {
            id,
            created_at: env.ledger().timestamp(),
            price_timestamp: env.ledger().timestamp(),
            executed: false,
            account: account.clone(),
            market: market.clone(),
            order_type,
            is_long,
            collateral_amount,
            collateral_token: market_config.short_token,
            size_delta_usd,
            acceptable_price,
            trigger_price,
            execution_fee,
            isolated,
            frozen: false,
            cancelled: false,
            twap_interval,
            twap_parts: parts,
            last_slice_ts: 0,
            remaining_usd: size_delta_usd,
        };
        save_order(&env, &o);
        env.storage().persistent().set(&pending_key, &id);
        bump_key(&env, &pending_key);
        OrderCreated {
            id,
            timestamp: o.created_at,
            market,
            account,
            order_type,
        }
        .publish(&env);
        bump(&env);
        id
    }

    pub fn cancel_order(env: Env, caller: Address, id: u32) {
        let c = cfg(&env);
        let mut o = load_order(&env, id);
        caller.require_auth();
        if caller != o.account {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if o.cancelled {
            panic_with_error!(&env, Error::Cancelled);
        }
        if o.executed {
            panic_with_error!(&env, Error::Executed);
        }
        o.cancelled = true;
        env.storage().persistent().remove(&DataKey::Pending(
            o.account.clone(),
            o.market.clone(),
            o.is_long,
        ));
        if is_increase(o.order_type) && o.collateral_amount > 0 {
            token::TokenClient::new(&env, &o.collateral_token).transfer(
                &env.current_contract_address(),
                &o.account,
                &o.collateral_amount,
            );
            o.collateral_amount = 0;
        }
        let fee = o.execution_fee;
        o.execution_fee = 0;
        save_order(&env, &o);
        if fee > 0 {
            token::TokenClient::new(&env, &c.fee_token).transfer(
                &env.current_contract_address(),
                &o.account,
                &fee,
            );
        }
        bump(&env);
    }

    pub fn freeze_order(env: Env, id: u32) {
        let c = cfg(&env);
        require_admin(&c);
        let mut o = load_order(&env, id);
        o.frozen = true;
        save_order(&env, &o);
        bump(&env);
    }

    pub fn update_order(
        env: Env,
        caller: Address,
        id: u32,
        acceptable_price: i128,
        trigger_price: i128,
        size_delta_usd: i128,
    ) {
        let mut o = load_order(&env, id);
        caller.require_auth();
        if caller != o.account {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if o.cancelled || o.executed || o.frozen {
            panic_with_error!(&env, Error::Frozen);
        }
        if acceptable_price <= 0 || trigger_price < 0 || size_delta_usd < 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        // An amendment is a new price request, never a way to select old prices.
        o.price_timestamp = env.ledger().timestamp();
        o.acceptable_price = acceptable_price;
        o.trigger_price = trigger_price;
        if size_delta_usd > 0 {
            o.size_delta_usd = size_delta_usd;
            o.remaining_usd = size_delta_usd;
        }
        save_order(&env, &o);
        bump(&env);
    }

    /// Validate the request against its exact timestamp. quote_at also resolves
    /// a configured source on demand, so this call never accepts a price.
    pub fn execute_order(env: Env, keeper: Address, id: u32) {
        keeper.require_auth();
        let c = cfg(&env);
        let allowed: bool = env
            .storage()
            .instance()
            .get(&DataKey::Keeper(keeper.clone()))
            .unwrap_or(false);
        if keeper != c.admin && !allowed {
            panic_with_error!(&env, Error::Unauthorized);
        }
        let mut o = load_order(&env, id);
        if o.cancelled {
            panic_with_error!(&env, Error::Cancelled);
        }
        if o.executed {
            panic_with_error!(&env, Error::Executed);
        }
        if o.frozen {
            panic_with_error!(&env, Error::Frozen);
        }
        let mcfg = validated_market(&env, &c, &o.market, is_increase(o.order_type));
        if mcfg.short_token != o.collateral_token {
            panic_with_error!(&env, Error::InvalidMarket);
        }
        if env.ledger().timestamp() < o.price_timestamp {
            panic_with_error!(&env, Error::NotReady);
        }
        let q = OracleContractClient::new(&env, &c.oracle)
            .quote_at(&mcfg.index_token, &o.price_timestamp);
        if !triggered(o.order_type, o.is_long, q.min, q.max, o.trigger_price) {
            panic_with_error!(&env, Error::NotReady);
        }
        let size = if is_twap(o.order_type) {
            twap_slice(o.remaining_usd.max(1), o.twap_parts.max(1)).unwrap_or(o.remaining_usd)
        } else {
            o.remaining_usd
        };
        let collat = if is_increase(o.order_type) {
            o.collateral_amount
        } else {
            0
        };
        let market = MarketContractClient::new(&env, &o.market);
        if is_increase(o.order_type) {
            if collat > 0 {
                token::TokenClient::new(&env, &mcfg.short_token).transfer(
                    &env.current_contract_address(),
                    &mcfg.pool,
                    &collat,
                );
            }
            market.increase(
                &o.account,
                &o.is_long,
                &collat,
                &size,
                &o.acceptable_price,
                &o.isolated,
                &o.price_timestamp,
            );
            o.collateral_amount = 0;
        } else {
            market.decrease(
                &o.account,
                &o.is_long,
                &o.collateral_amount,
                &size,
                &o.acceptable_price,
                &o.account,
                &o.price_timestamp,
            );
            o.collateral_amount = 0;
        }
        o.remaining_usd = o.remaining_usd.saturating_sub(size);
        o.last_slice_ts = o.price_timestamp;
        if is_twap(o.order_type) && o.twap_parts > 1 {
            o.twap_parts -= 1;
        }
        let done = o.remaining_usd <= 0 || !is_twap(o.order_type);
        if o.execution_fee > 0 {
            token::TokenClient::new(&env, &c.fee_token).transfer(
                &env.current_contract_address(),
                &keeper,
                &o.execution_fee,
            );
            o.execution_fee = 0;
        }
        let executed_timestamp = o.price_timestamp;
        if done {
            o.executed = true;
            env.storage().persistent().remove(&DataKey::Pending(
                o.account.clone(),
                o.market.clone(),
                o.is_long,
            ));
        } else {
            o.price_timestamp = o
                .price_timestamp
                .checked_add(o.twap_interval)
                .unwrap_or_else(|| panic_with_error!(&env, Error::InvalidAmount));
        }
        save_order(&env, &o);
        OrderExecuted {
            id,
            timestamp: executed_timestamp,
            keeper,
            size_usd: size,
        }
        .publish(&env);
        bump(&env);
    }

    pub fn get_order(env: Env, id: u32) -> Order {
        load_order(&env, id)
    }

    pub fn config(env: Env) -> Config {
        cfg(&env)
    }
}

#[cfg(test)]
mod test;
