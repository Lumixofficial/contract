#![no_std]

use lumix_core::{
    account_is_liquidatable, accrue_borrow, accrue_funding, apply_impact_to_price,
    blend_entry_price, bps_of_ceil, bps_of_floor, exceeds_max_leverage, from_usd, is_liquidatable,
    position_borrow_usd, position_fee_usd, position_funding_usd, position_pnl_usd,
    price_acceptable, price_impact_with_rebate, remaining_margin_usd, should_adl,
    split_protocol_fee, to_usd, BorrowState, FundingState, DEFAULT_ADL_PNL_FACTOR_BPS,
    DEFAULT_LIQ_FEE_BPS, DEFAULT_MAX_LEVERAGE, DEFAULT_MAX_OI_FACTOR_BPS, DEFAULT_MMR_BPS,
    DEFAULT_POSITION_FEE_BALANCED_BPS, DEFAULT_POSITION_FEE_IMBALANCED_BPS, DEFAULT_REBATE_DELAY,
    INSTANCE_TTL_EXTEND_TO, INSTANCE_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO,
    PERSISTENT_TTL_THRESHOLD,
};
use lumix_interfaces::{OracleContractClient, PoolContractClient};
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
    Paused = 2,
    InvalidAmount = 3,
    MaxLeverage = 4,
    OiCap = 5,
    Slippage = 6,
    NotLiquidatable = 7,
    NoPosition = 8,
    Math = 9,
    TradingDisabled = 10,
    IsolatedOnly = 11,
    InvalidTimestamp = 12,
    OracleMismatch = 13,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MarketConfig {
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub account: Address,
    pub is_long: bool,
    pub size_usd: i128,
    pub collateral_amount: i128,
    pub entry_price: i128,
    pub funding_cp: i128,
    pub borrow_cp: i128,
    pub reserved: i128,
    pub isolated: bool,
    pub opened_at: u64,
    pub updated_at: u64,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct OpenInterest {
    pub long_usd: i128,
    pub short_usd: i128,
    pub long_avg_entry: i128,
    pub short_avg_entry: i128,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CrossAccount {
    pub collateral_amount: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundingStore {
    pub saved_factor_per_second: i128,
    pub last_timestamp: u64,
    pub cumulative_long: i128,
    pub cumulative_short: i128,
}

impl FundingStore {
    fn new(now: u64) -> Self {
        Self {
            saved_factor_per_second: 0,
            last_timestamp: now,
            cumulative_long: 0,
            cumulative_short: 0,
        }
    }
    fn to_core(&self) -> FundingState {
        FundingState {
            saved_factor_per_second: self.saved_factor_per_second,
            last_timestamp: self.last_timestamp,
            cumulative_long: self.cumulative_long,
            cumulative_short: self.cumulative_short,
        }
    }
    fn from_core(s: FundingState) -> Self {
        Self {
            saved_factor_per_second: s.saved_factor_per_second,
            last_timestamp: s.last_timestamp,
            cumulative_long: s.cumulative_long,
            cumulative_short: s.cumulative_short,
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BorrowStore {
    pub cumulative_long: i128,
    pub cumulative_short: i128,
    pub last_timestamp: u64,
}

impl BorrowStore {
    fn new(now: u64) -> Self {
        Self {
            cumulative_long: 0,
            cumulative_short: 0,
            last_timestamp: now,
        }
    }
    fn to_core(&self) -> BorrowState {
        BorrowState {
            cumulative_long: self.cumulative_long,
            cumulative_short: self.cumulative_short,
            last_timestamp: self.last_timestamp,
        }
    }
    fn from_core(s: BorrowState) -> Self {
        Self {
            cumulative_long: s.cumulative_long,
            cumulative_short: s.cumulative_short,
            last_timestamp: s.last_timestamp,
        }
    }
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    Funding,
    Borrow,
    Oi,
    Position(Address, bool),
    Cross(Address),
    VirtualLong,
    VirtualShort,
    SkipAuth,
    Keeper(Address),
    LastAction(Address, bool),
    Rebate(Address),
}

#[contractevent]
pub struct PositionIncreased {
    pub account: Address,
    pub is_long: bool,
    pub size_usd: i128,
    pub collateral: i128,
    pub price: i128,
}

#[contractevent]
pub struct PositionDecreased {
    pub account: Address,
    pub is_long: bool,
    pub size_usd: i128,
    pub pnl: i128,
    pub price: i128,
}

#[contractevent]
pub struct Liquidated {
    pub account: Address,
    pub is_long: bool,
    pub size_usd: i128,
    pub fee: i128,
}

#[contractevent]
pub struct AdlExecuted {
    pub account: Address,
    pub is_long: bool,
    pub size_usd: i128,
}

#[contract]
pub struct Market;

fn cfg(env: &Env) -> MarketConfig {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized))
}

fn save_cfg(env: &Env, c: &MarketConfig) {
    env.storage().instance().set(&DataKey::Config, c);
}

fn oi(env: &Env) -> OpenInterest {
    env.storage()
        .instance()
        .get(&DataKey::Oi)
        .unwrap_or(OpenInterest {
            long_usd: 0,
            short_usd: 0,
            long_avg_entry: 0,
            short_avg_entry: 0,
        })
}

fn save_oi(env: &Env, o: &OpenInterest) {
    env.storage().instance().set(&DataKey::Oi, o);
}

fn funding(env: &Env) -> FundingStore {
    env.storage()
        .instance()
        .get(&DataKey::Funding)
        .unwrap_or(FundingStore::new(env.ledger().timestamp()))
}

fn borrow(env: &Env) -> BorrowStore {
    env.storage()
        .instance()
        .get(&DataKey::Borrow)
        .unwrap_or(BorrowStore::new(env.ledger().timestamp()))
}

fn require_exchange(env: &Env, c: &MarketConfig) {
    if env.storage().temporary().has(&DataKey::SkipAuth) {
        return;
    }
    let ex = c
        .exchange
        .clone()
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized));
    ex.require_auth();
}

fn require_admin(c: &MarketConfig) {
    c.admin.require_auth();
}

fn index_price(env: &Env, c: &MarketConfig, prefer_max: bool, timestamp: Option<u64>) -> i128 {
    let o = quote(env, c, &c.index_token, timestamp);
    if prefer_max {
        o.max
    } else {
        o.min
    }
}

fn token_price(env: &Env, c: &MarketConfig, token: &Address, timestamp: Option<u64>) -> i128 {
    quote(env, c, token, timestamp).mid()
}

fn accrue(env: &Env, c: &MarketConfig, timestamp: Option<u64>) {
    let o = oi(env);
    let now = env.ledger().timestamp();
    let f = accrue_funding(funding(env).to_core(), o.long_usd, o.short_usd, now)
        .unwrap_or_else(|_| panic_with_error!(env, Error::Math));
    env.storage()
        .instance()
        .set(&DataKey::Funding, &FundingStore::from_core(f));
    let pool = PoolContractClient::new(env, &c.pool);
    let st = pool.get_state();
    let pool_usd = pool_value(env, c, timestamp);
    let reserved = st.reserved_long.saturating_add(st.reserved_short);
    let b = accrue_borrow(
        borrow(env).to_core(),
        o.long_usd,
        o.short_usd,
        reserved,
        pool_usd.max(1),
        now,
    )
    .unwrap_or_else(|_| panic_with_error!(env, Error::Math));
    env.storage()
        .instance()
        .set(&DataKey::Borrow, &BorrowStore::from_core(b));
}

fn pos_key(account: &Address, is_long: bool) -> DataKey {
    DataKey::Position(account.clone(), is_long)
}

fn load_pos(env: &Env, account: &Address, is_long: bool) -> Option<Position> {
    env.storage().persistent().get(&pos_key(account, is_long))
}

fn save_pos(env: &Env, p: &Position) {
    let key = pos_key(&p.account, p.is_long);
    env.storage().persistent().set(&key, p);
    bump_key(env, &key);
}

fn del_pos(env: &Env, account: &Address, is_long: bool) {
    env.storage()
        .persistent()
        .remove(&pos_key(account, is_long));
}

fn load_cross(env: &Env, account: &Address) -> CrossAccount {
    env.storage()
        .persistent()
        .get(&DataKey::Cross(account.clone()))
        .unwrap_or(CrossAccount {
            collateral_amount: 0,
        })
}

fn save_cross(env: &Env, account: &Address, a: &CrossAccount) {
    let key = DataKey::Cross(account.clone());
    env.storage().persistent().set(&key, a);
    bump_key(env, &key);
}

fn net_pending_pnl(env: &Env, c: &MarketConfig, timestamp: Option<u64>) -> i128 {
    let o = oi(env);
    let exec_long = exec_price(c, env, true, false, timestamp);
    let exec_short = exec_price(c, env, false, false, timestamp);
    let mut pnl = 0i128;
    if o.long_usd > 0 && o.long_avg_entry > 0 {
        pnl = pnl.saturating_add(
            position_pnl_usd(o.long_usd, o.long_avg_entry, exec_long, true).unwrap_or(0),
        );
    }
    if o.short_usd > 0 && o.short_avg_entry > 0 {
        pnl = pnl.saturating_add(
            position_pnl_usd(o.short_usd, o.short_avg_entry, exec_short, false).unwrap_or(0),
        );
    }
    pnl
}

fn settle_fees(p: &Position, f: &FundingState, b: &BorrowState) -> i128 {
    let fund = position_funding_usd(p.size_usd, p.is_long, p.funding_cp, f).unwrap_or(0);
    let bor = position_borrow_usd(p.size_usd, p.is_long, p.borrow_cp, b).unwrap_or(0);
    fund.saturating_add(bor)
}

fn max_lev(c: &MarketConfig) -> u32 {
    if c.on_hours {
        c.max_leverage
    } else {
        c.off_hours_max_leverage
    }
}

fn mmr(c: &MarketConfig) -> u32 {
    if c.on_hours {
        c.mmr_bps
    } else {
        c.off_hours_mmr_bps
    }
}

fn exec_price(
    c: &MarketConfig,
    env: &Env,
    is_long: bool,
    is_increase: bool,
    timestamp: Option<u64>,
) -> i128 {
    // Longs open at max, close at min; shorts the reverse.
    let prefer_max = is_long == is_increase;
    index_price(env, c, prefer_max, timestamp)
}

fn virtual_oi(env: &Env) -> (i128, i128) {
    let vl: i128 = env
        .storage()
        .instance()
        .get(&DataKey::VirtualLong)
        .unwrap_or(0);
    let vs: i128 = env
        .storage()
        .instance()
        .get(&DataKey::VirtualShort)
        .unwrap_or(0);
    (vl, vs)
}

fn add_virtual(env: &Env, is_long: bool, delta: i128) {
    let (mut l, mut s) = virtual_oi(env);
    if is_long {
        l = l.saturating_add(delta);
    } else {
        s = s.saturating_add(delta);
    }
    env.storage()
        .instance()
        .set(&DataKey::VirtualLong, &l.max(0));
    env.storage()
        .instance()
        .set(&DataKey::VirtualShort, &s.max(0));
}

fn impact_for(
    env: &Env,
    c: &MarketConfig,
    o: &OpenInterest,
    is_long: bool,
    size_delta: i128,
    increase: bool,
) -> i128 {
    let signed = if increase { size_delta } else { -size_delta };
    let next_l = if is_long {
        o.long_usd.saturating_add(signed)
    } else {
        o.long_usd
    };
    let next_s = if is_long {
        o.short_usd
    } else {
        o.short_usd.saturating_add(signed)
    };
    let (vl, vs) = virtual_oi(env);
    let (i_real, excess_real) = price_impact_with_rebate(
        o.long_usd,
        o.short_usd,
        next_l.max(0),
        next_s.max(0),
        size_delta,
        c.impact_factor_bps,
        c.impact_cap_bps,
    )
    .unwrap_or((0, 0));
    let (i_virt, excess_virt) = price_impact_with_rebate(
        o.long_usd.max(vl),
        o.short_usd.max(vs),
        next_l.max(0).max(vl),
        next_s.max(0).max(vs),
        size_delta,
        c.impact_factor_bps,
        c.impact_cap_bps,
    )
    .unwrap_or((0, 0));
    let _ = (excess_real, excess_virt);
    // Conservative: take the worse (more positive) impact.
    i_real.max(i_virt)
}

fn impact_excess(
    env: &Env,
    c: &MarketConfig,
    o: &OpenInterest,
    is_long: bool,
    size_delta: i128,
    increase: bool,
) -> i128 {
    let signed = if increase { size_delta } else { -size_delta };
    let next_l = if is_long {
        o.long_usd.saturating_add(signed)
    } else {
        o.long_usd
    };
    let next_s = if is_long {
        o.short_usd
    } else {
        o.short_usd.saturating_add(signed)
    };
    let (_, excess) = price_impact_with_rebate(
        o.long_usd,
        o.short_usd,
        next_l.max(0),
        next_s.max(0),
        size_delta,
        c.impact_factor_bps,
        c.impact_cap_bps,
    )
    .unwrap_or((0, 0));
    let _ = env;
    excess
}

fn credit_rebate(env: &Env, account: &Address, excess_usd: i128) {
    if excess_usd <= 0 {
        return;
    }
    let key = DataKey::Rebate(account.clone());
    let (amt, _when): (i128, u64) = env.storage().persistent().get(&key).unwrap_or((0, 0));
    let available_at = env
        .ledger()
        .timestamp()
        .saturating_add(DEFAULT_REBATE_DELAY);
    env.storage()
        .persistent()
        .set(&key, &(amt.saturating_add(excess_usd), available_at));
    bump_key(env, &key);
}

fn pay_from_collateral(amount_tokens: i128, collat: i128) -> (i128, i128) {
    let paid = amount_tokens.min(collat).max(0);
    (collat - paid, paid)
}

fn usd_of_token(
    env: &Env,
    c: &MarketConfig,
    token: &Address,
    amount: i128,
    timestamp: Option<u64>,
) -> i128 {
    to_usd(amount, token_price(env, c, token, timestamp)).unwrap_or(0)
}

fn tokens_of_usd(
    env: &Env,
    c: &MarketConfig,
    token: &Address,
    usd: i128,
    timestamp: Option<u64>,
) -> i128 {
    from_usd(usd.max(0), token_price(env, c, token, timestamp)).unwrap_or(0)
}

fn quote(
    env: &Env,
    c: &MarketConfig,
    asset: &Address,
    timestamp: Option<u64>,
) -> lumix_interfaces::OraclePrice {
    let oracle = OracleContractClient::new(env, &c.oracle);
    match timestamp {
        Some(ts) => oracle.quote_at(asset, &ts),
        None => oracle.quote(asset),
    }
}

fn pool_value(env: &Env, c: &MarketConfig, timestamp: Option<u64>) -> i128 {
    let pool = PoolContractClient::new(env, &c.pool);
    match timestamp {
        Some(ts) => pool.pool_value_at(&ts),
        None => pool.pool_value(),
    }
}

fn validate_action_time(env: &Env, account: &Address, is_long: bool, timestamp: u64) {
    let key = DataKey::LastAction(account.clone(), is_long);
    let last: u64 = env.storage().persistent().get(&key).unwrap_or(0);
    if timestamp > env.ledger().timestamp() || timestamp < last {
        panic_with_error!(env, Error::InvalidTimestamp);
    }
}

fn save_action_time(env: &Env, account: &Address, is_long: bool, timestamp: u64) {
    let key = DataKey::LastAction(account.clone(), is_long);
    env.storage().persistent().set(&key, &timestamp);
    bump_key(env, &key);
}

fn validate_liquidation_time(env: &Env, account: &Address, is_long: bool, timestamp: u64) {
    validate_action_time(env, account, is_long, timestamp);
    if env.ledger().timestamp() - timestamp > lumix_core::DEFAULT_ORACLE_MAX_AGE {
        panic_with_error!(env, Error::InvalidTimestamp);
    }
}

fn require_keeper(env: &Env, c: &MarketConfig, keeper: &Address) {
    keeper.require_auth();
    if *keeper != c.admin
        && !env
            .storage()
            .instance()
            .get(&DataKey::Keeper(keeper.clone()))
            .unwrap_or(false)
    {
        panic_with_error!(env, Error::Unauthorized);
    }
}

fn liquidate_position(
    env: Env,
    account: Address,
    is_long: bool,
    liquidator: Address,
    timestamp: u64,
) -> i128 {
    let c = cfg(&env);
    validate_liquidation_time(&env, &account, is_long, timestamp);
    save_action_time(&env, &account, is_long, timestamp);
    let timestamp = Some(timestamp);
    accrue(&env, &c, timestamp);
    let p = load_pos(&env, &account, is_long)
        .unwrap_or_else(|| panic_with_error!(&env, Error::NoPosition));
    let exec = exec_price(&c, &env, is_long, false, timestamp);
    let f = funding(&env).to_core();
    let b = borrow(&env).to_core();
    let fees = settle_fees(&p, &f, &b).max(0);
    let pnl = position_pnl_usd(p.size_usd, p.entry_price, exec, is_long).unwrap_or(0);
    let collat_usd = usd_of_token(&env, &c, &c.short_token, p.collateral_amount, timestamp);
    if !is_liquidatable(collat_usd, pnl, fees, p.size_usd, mmr(&c)).unwrap_or(false) {
        panic_with_error!(&env, Error::NotLiquidatable);
    }
    let liq_fee = bps_of_ceil(collat_usd.max(0), c.liq_fee_bps).unwrap_or(0);
    let mut o = oi(&env);
    if is_long {
        o.long_usd = o.long_usd.saturating_sub(p.size_usd);
        if o.long_usd == 0 {
            o.long_avg_entry = 0;
        }
    } else {
        o.short_usd = o.short_usd.saturating_sub(p.size_usd);
        if o.short_usd == 0 {
            o.short_avg_entry = 0;
        }
    }
    add_virtual(&env, is_long, -p.size_usd);
    PoolContractClient::new(&env, &c.pool).release(&is_long, &p.reserved);
    save_oi(&env, &o);
    del_pos(&env, &account, is_long);
    PoolContractClient::new(&env, &c.pool).adjust_collateral(&-p.collateral_amount);
    let leftover_usd = remaining_margin_usd(collat_usd, pnl, fees + liq_fee).unwrap_or(0);
    let leftover = if leftover_usd > 0 {
        tokens_of_usd(&env, &c, &c.short_token, leftover_usd, timestamp)
    } else {
        0
    };
    if leftover > 0 {
        PoolContractClient::new(&env, &c.pool).pay(&account, &c.short_token, &leftover);
    }
    let liq_tokens = tokens_of_usd(&env, &c, &c.short_token, liq_fee, timestamp);
    if liq_tokens > 0 {
        PoolContractClient::new(&env, &c.pool).pay(&liquidator, &c.short_token, &(liq_tokens / 2));
    }
    PoolContractClient::new(&env, &c.pool).set_pending_pnl(&net_pending_pnl(&env, &c, timestamp));
    Liquidated {
        account,
        is_long,
        size_usd: p.size_usd,
        fee: liq_fee,
    }
    .publish(&env);
    bump(&env);
    leftover
}

#[contractimpl]
impl Market {
    pub fn __constructor(
        env: Env,
        admin: Address,
        pool: Address,
        oracle: Address,
        index_token: Address,
        fee_receiver: Address,
    ) {
        let p = PoolContractClient::new(&env, &pool);
        if p.oracle() != oracle {
            panic_with_error!(&env, Error::OracleMismatch);
        }
        let c = MarketConfig {
            admin,
            exchange: None,
            pool,
            oracle,
            index_token,
            long_token: p.long_token(),
            short_token: p.short_token(),
            fee_receiver,
            paused: false,
            trading_enabled: true,
            synthetic: false,
            max_leverage: DEFAULT_MAX_LEVERAGE,
            mmr_bps: DEFAULT_MMR_BPS,
            liq_fee_bps: DEFAULT_LIQ_FEE_BPS,
            position_fee_balanced_bps: DEFAULT_POSITION_FEE_BALANCED_BPS,
            position_fee_imbalanced_bps: DEFAULT_POSITION_FEE_IMBALANCED_BPS,
            protocol_fee_bps: 2_000,
            max_oi_long_usd: 0,
            max_oi_short_usd: 0,
            max_oi_factor_bps: DEFAULT_MAX_OI_FACTOR_BPS,
            impact_factor_bps: 10,
            impact_cap_bps: 50,
            adl_factor_bps: DEFAULT_ADL_PNL_FACTOR_BPS,
            off_hours_max_leverage: 10,
            off_hours_mmr_bps: 800,
            on_hours: true,
        };
        save_cfg(&env, &c);
        save_oi(
            &env,
            &OpenInterest {
                long_usd: 0,
                short_usd: 0,
                long_avg_entry: 0,
                short_avg_entry: 0,
            },
        );
        env.storage().instance().set(
            &DataKey::Funding,
            &FundingStore::new(env.ledger().timestamp()),
        );
        env.storage().instance().set(
            &DataKey::Borrow,
            &BorrowStore::new(env.ledger().timestamp()),
        );
        bump(&env);
    }

    pub fn set_exchange(env: Env, exchange: Address) {
        let mut c = cfg(&env);
        require_admin(&c);
        c.exchange = Some(exchange);
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn set_keeper(env: Env, keeper: Address, allowed: bool) {
        require_admin(&cfg(&env));
        env.storage()
            .instance()
            .set(&DataKey::Keeper(keeper), &allowed);
        bump(&env);
    }

    pub fn set_status(env: Env, paused: bool, trading_enabled: bool, on_hours: bool) {
        let mut c = cfg(&env);
        require_admin(&c);
        c.paused = paused;
        c.trading_enabled = trading_enabled;
        c.on_hours = on_hours;
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn set_risk(
        env: Env,
        max_leverage: u32,
        mmr_bps: u32,
        max_oi_long_usd: i128,
        max_oi_short_usd: i128,
        synthetic: bool,
    ) {
        let mut c = cfg(&env);
        require_admin(&c);
        c.max_leverage = max_leverage;
        c.mmr_bps = mmr_bps;
        c.max_oi_long_usd = max_oi_long_usd;
        c.max_oi_short_usd = max_oi_short_usd;
        c.synthetic = synthetic;
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn increase(
        env: Env,
        account: Address,
        is_long: bool,
        collateral_amount: i128,
        size_delta_usd: i128,
        acceptable_price: i128,
        isolated: bool,
        timestamp: u64,
    ) -> Position {
        validate_action_time(&env, &account, is_long, timestamp);
        let request_timestamp = timestamp;
        let timestamp = Some(timestamp);
        let c = cfg(&env);
        require_exchange(&env, &c);
        if c.paused || !c.trading_enabled {
            panic_with_error!(&env, Error::TradingDisabled);
        }
        if size_delta_usd < 0
            || collateral_amount < 0
            || (size_delta_usd == 0 && collateral_amount == 0)
        {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        if isolated && collateral_amount > 0 {
            PoolContractClient::new(&env, &c.pool).adjust_collateral(&collateral_amount);
        }
        accrue(&env, &c, timestamp);
        let mut o = oi(&env);
        let f = funding(&env).to_core();
        let b = borrow(&env).to_core();
        let mut p = load_pos(&env, &account, is_long).unwrap_or(Position {
            account: account.clone(),
            is_long,
            size_usd: 0,
            collateral_amount: 0,
            entry_price: 0,
            funding_cp: if is_long {
                f.cumulative_long
            } else {
                f.cumulative_short
            },
            borrow_cp: if is_long {
                b.cumulative_long
            } else {
                b.cumulative_short
            },
            reserved: 0,
            isolated,
            opened_at: request_timestamp,
            updated_at: request_timestamp,
        });
        if p.size_usd > 0 && p.isolated != isolated {
            panic_with_error!(&env, Error::IsolatedOnly);
        }
        let fees_usd = settle_fees(&p, &f, &b);
        let fee_tokens = tokens_of_usd(&env, &c, &c.short_token, fees_usd.max(0), timestamp);
        let (left, paid) = pay_from_collateral(fee_tokens, p.collateral_amount);
        PoolContractClient::new(&env, &c.pool).adjust_collateral(&-paid);
        p.collateral_amount = left;
        p.funding_cp = if is_long {
            f.cumulative_long
        } else {
            f.cumulative_short
        };
        p.borrow_cp = if is_long {
            b.cumulative_long
        } else {
            b.cumulative_short
        };

        if collateral_amount > 0 {
            if !isolated {
                let mut x = load_cross(&env, &account);
                if x.collateral_amount < collateral_amount {
                    panic_with_error!(&env, Error::InvalidAmount);
                }
                x.collateral_amount -= collateral_amount;
                save_cross(&env, &account, &x);
            }
            p.collateral_amount += collateral_amount;
        }

        let mut exec = exec_price(&c, &env, is_long, true, timestamp);
        if size_delta_usd > 0 {
            let impact = impact_for(&env, &c, &o, is_long, size_delta_usd, true);
            exec =
                apply_impact_to_price(exec, impact, size_delta_usd, is_long, true).unwrap_or(exec);
            if !price_acceptable(exec, acceptable_price, is_long, true).unwrap_or(false) {
                panic_with_error!(&env, Error::Slippage);
            }
            credit_rebate(
                &env,
                &account,
                impact_excess(&env, &c, &o, is_long, size_delta_usd, true),
            );
            let (fee, _) = position_fee_usd(
                size_delta_usd,
                o.long_usd,
                o.short_usd,
                is_long,
                c.position_fee_balanced_bps,
                c.position_fee_imbalanced_bps,
            )
            .unwrap_or((0, false));
            let (pool_fee, proto) = split_protocol_fee(fee, c.protocol_fee_bps).unwrap_or((fee, 0));
            let fee_t = tokens_of_usd(&env, &c, &c.short_token, pool_fee + proto, timestamp);
            let (left2, paid) = pay_from_collateral(fee_t, p.collateral_amount);
            PoolContractClient::new(&env, &c.pool).adjust_collateral(&-paid);
            p.collateral_amount = left2;
            if proto > 0 {
                let proto_t = tokens_of_usd(&env, &c, &c.short_token, proto, timestamp);
                PoolContractClient::new(&env, &c.pool).pay(
                    &c.fee_receiver,
                    &c.short_token,
                    &proto_t,
                );
            }
            p.entry_price =
                blend_entry_price(p.size_usd, p.entry_price, size_delta_usd, exec).unwrap_or(exec);
            p.size_usd += size_delta_usd;
            if is_long {
                o.long_avg_entry = if o.long_usd == 0 {
                    exec
                } else {
                    blend_entry_price(o.long_usd, o.long_avg_entry, size_delta_usd, exec)
                        .unwrap_or(exec)
                };
                o.long_usd += size_delta_usd;
            } else {
                o.short_avg_entry = if o.short_usd == 0 {
                    exec
                } else {
                    blend_entry_price(o.short_usd, o.short_avg_entry, size_delta_usd, exec)
                        .unwrap_or(exec)
                };
                o.short_usd += size_delta_usd;
            }
            add_virtual(&env, is_long, size_delta_usd);
            let reserve_amt = if is_long && !c.synthetic {
                from_usd(size_delta_usd, exec).unwrap_or(0)
            } else {
                tokens_of_usd(&env, &c, &c.short_token, size_delta_usd, timestamp)
            };
            PoolContractClient::new(&env, &c.pool).reserve(&is_long, &reserve_amt);
            p.reserved += reserve_amt;
        }

        let extra = if isolated {
            0
        } else {
            usd_of_token(
                &env,
                &c,
                &c.short_token,
                load_cross(&env, &account).collateral_amount,
                timestamp,
            )
        };
        let collat_usd =
            usd_of_token(&env, &c, &c.short_token, p.collateral_amount, timestamp) + extra;
        if exceeds_max_leverage(p.size_usd, collat_usd.max(1), max_lev(&c)).unwrap_or(true) {
            panic_with_error!(&env, Error::MaxLeverage);
        }
        let pool_usd = pool_value(&env, &c, timestamp);
        let implicit = bps_of_floor(pool_usd.max(1), c.max_oi_factor_bps).unwrap_or(0);
        let cap = if is_long {
            c.max_oi_long_usd
        } else {
            c.max_oi_short_usd
        };
        let cap = if cap > 0 { cap } else { implicit };
        let next_oi = if is_long { o.long_usd } else { o.short_usd };
        if cap > 0 && next_oi > cap {
            panic_with_error!(&env, Error::OiCap);
        }
        p.updated_at = request_timestamp;
        save_action_time(&env, &account, is_long, request_timestamp);
        save_oi(&env, &o);
        save_pos(&env, &p);
        PoolContractClient::new(&env, &c.pool)
            .set_pending_pnl(&net_pending_pnl(&env, &c, timestamp));
        PositionIncreased {
            account,
            is_long,
            size_usd: p.size_usd,
            collateral: p.collateral_amount,
            price: exec,
        }
        .publish(&env);
        bump(&env);
        p
    }

    pub fn decrease(
        env: Env,
        account: Address,
        is_long: bool,
        collateral_withdrawal: i128,
        size_delta_usd: i128,
        acceptable_price: i128,
        receiver: Address,
        timestamp: u64,
    ) -> i128 {
        validate_action_time(&env, &account, is_long, timestamp);
        let request_timestamp = timestamp;
        let timestamp = Some(timestamp);
        let c = cfg(&env);
        require_exchange(&env, &c);
        if size_delta_usd < 0 || collateral_withdrawal < 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        if c.paused {
            panic_with_error!(&env, Error::Paused);
        }
        accrue(&env, &c, timestamp);
        let mut p = load_pos(&env, &account, is_long)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPosition));
        let previous_collateral = p.collateral_amount;
        let mut o = oi(&env);
        let f = funding(&env).to_core();
        let b = borrow(&env).to_core();
        let mut fees_usd = settle_fees(&p, &f, &b);
        p.funding_cp = if is_long {
            f.cumulative_long
        } else {
            f.cumulative_short
        };
        p.borrow_cp = if is_long {
            b.cumulative_long
        } else {
            b.cumulative_short
        };

        p.updated_at = request_timestamp;
        save_action_time(&env, &account, is_long, request_timestamp);
        let size_close = size_delta_usd.min(p.size_usd).max(0);
        let mut exec = exec_price(&c, &env, is_long, false, timestamp);
        let mut pnl_usd = 0i128;
        if size_close > 0 {
            let impact = impact_for(&env, &c, &o, is_long, size_close, false);
            exec = apply_impact_to_price(exec, impact, size_close, is_long, false).unwrap_or(exec);
            if !price_acceptable(exec, acceptable_price, is_long, false).unwrap_or(false) {
                panic_with_error!(&env, Error::Slippage);
            }
            credit_rebate(
                &env,
                &account,
                impact_excess(&env, &c, &o, is_long, size_close, false),
            );
            pnl_usd = position_pnl_usd(size_close, p.entry_price, exec, is_long).unwrap_or(0);
            let (fee, _) = position_fee_usd(
                size_close,
                o.long_usd,
                o.short_usd,
                is_long,
                c.position_fee_balanced_bps,
                c.position_fee_imbalanced_bps,
            )
            .unwrap_or((0, false));
            fees_usd = fees_usd.saturating_add(fee);
            let release = if p.size_usd == 0 {
                0
            } else {
                p.reserved * size_close / p.size_usd
            };
            PoolContractClient::new(&env, &c.pool).release(&is_long, &release);
            p.reserved -= release;
            p.size_usd -= size_close;
            if is_long {
                o.long_usd = o.long_usd.saturating_sub(size_close);
                if o.long_usd == 0 {
                    o.long_avg_entry = 0;
                }
            } else {
                o.short_usd = o.short_usd.saturating_sub(size_close);
                if o.short_usd == 0 {
                    o.short_avg_entry = 0;
                }
            }
            add_virtual(&env, is_long, -size_close);
        }
        let mut remaining_usd = remaining_margin_usd(
            usd_of_token(&env, &c, &c.short_token, p.collateral_amount, timestamp),
            pnl_usd,
            fees_usd.max(0),
        )
        .unwrap_or(0);
        if remaining_usd < 0 {
            remaining_usd = 0;
        }
        let remaining_tokens = tokens_of_usd(&env, &c, &c.short_token, remaining_usd, timestamp);
        let payout = if p.size_usd == 0 {
            del_pos(&env, &account, is_long);
            remaining_tokens
        } else {
            let want = collateral_withdrawal.max(0).min(remaining_tokens);
            p.collateral_amount = remaining_tokens.saturating_sub(want);
            let collat_usd = usd_of_token(&env, &c, &c.short_token, p.collateral_amount, timestamp);
            if exceeds_max_leverage(p.size_usd, collat_usd.max(1), max_lev(&c)).unwrap_or(true) {
                panic_with_error!(&env, Error::MaxLeverage);
            }
            save_pos(&env, &p);
            want
        };
        let next_collateral = if p.size_usd == 0 {
            0
        } else {
            p.collateral_amount
        };
        PoolContractClient::new(&env, &c.pool)
            .adjust_collateral(&(next_collateral - previous_collateral));
        save_oi(&env, &o);
        PoolContractClient::new(&env, &c.pool).pay(&receiver, &c.short_token, &payout);
        PoolContractClient::new(&env, &c.pool)
            .set_pending_pnl(&net_pending_pnl(&env, &c, timestamp));
        PositionDecreased {
            account,
            is_long,
            size_usd: size_close,
            pnl: pnl_usd,
            price: exec,
        }
        .publish(&env);
        bump(&env);
        payout
    }

    pub fn liquidate(
        env: Env,
        account: Address,
        is_long: bool,
        liquidator: Address,
        timestamp: u64,
    ) -> i128 {
        let c = cfg(&env);
        require_keeper(&env, &c, &liquidator);
        liquidate_position(env, account, is_long, liquidator, timestamp)
    }

    pub fn adl(env: Env, account: Address, is_long: bool, size_usd: i128) {
        let timestamp = None;
        let c = cfg(&env);
        require_admin(&c);
        accrue(&env, &c, timestamp);
        let pool_usd = pool_value(&env, &c, timestamp);
        let pending = net_pending_pnl(&env, &c, timestamp);
        if !should_adl(pending, pool_usd, c.adl_factor_bps).unwrap_or(false) {
            panic_with_error!(&env, Error::NotLiquidatable);
        }
        env.storage().temporary().set(&DataKey::SkipAuth, &true);
        Self::decrease(
            env.clone(),
            account.clone(),
            is_long,
            0,
            size_usd,
            if is_long { 1 } else { i128::MAX / 4 },
            account.clone(),
            env.ledger().timestamp(),
        );
        env.storage().temporary().remove(&DataKey::SkipAuth);
        AdlExecuted {
            account,
            is_long,
            size_usd,
        }
        .publish(&env);
    }

    pub fn deposit_cross(env: Env, account: Address, amount: i128) {
        account.require_auth();
        let c = cfg(&env);
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        token::TokenClient::new(&env, &c.short_token).transfer(&account, &c.pool, &amount);
        let mut a = load_cross(&env, &account);
        a.collateral_amount += amount;
        PoolContractClient::new(&env, &c.pool).adjust_collateral(&amount);
        save_cross(&env, &account, &a);
        bump(&env);
    }

    pub fn withdraw_cross(env: Env, account: Address, amount: i128) -> i128 {
        account.require_auth();
        let c = cfg(&env);
        let mut a = load_cross(&env, &account);
        if amount <= 0 || amount > a.collateral_amount {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        a.collateral_amount -= amount;
        PoolContractClient::new(&env, &c.pool).adjust_collateral(&-amount);
        save_cross(&env, &account, &a);
        PoolContractClient::new(&env, &c.pool).pay(&account, &c.short_token, &amount);
        bump(&env);
        amount
    }

    pub fn claim_rebate(env: Env, account: Address) -> i128 {
        let timestamp = None;
        account.require_auth();
        let c = cfg(&env);
        let key = DataKey::Rebate(account.clone());
        let (amt, available_at): (i128, u64) =
            env.storage().persistent().get(&key).unwrap_or((0, 0));
        if amt <= 0 || env.ledger().timestamp() < available_at {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        env.storage().persistent().remove(&key);
        let tokens = tokens_of_usd(&env, &c, &c.short_token, amt, timestamp);
        if tokens > 0 {
            PoolContractClient::new(&env, &c.pool).pay(&account, &c.short_token, &tokens);
        }
        bump(&env);
        tokens
    }

    pub fn liquidate_account(
        env: Env,
        account: Address,
        liquidator: Address,
        timestamp: u64,
    ) -> i128 {
        let c = cfg(&env);
        require_keeper(&env, &c, &liquidator);
        let liquidation_timestamp = timestamp;
        let timestamp = Some(timestamp);
        accrue(&env, &c, timestamp);
        let long = load_pos(&env, &account, true);
        let short = load_pos(&env, &account, false);
        let x = load_cross(&env, &account);
        let mut collat = if x.collateral_amount > 0 {
            usd_of_token(&env, &c, &c.short_token, x.collateral_amount, timestamp)
        } else {
            0
        };
        let mut pnl = 0i128;
        let mut fees = 0i128;
        let mut size = 0i128;
        let f = funding(&env).to_core();
        let b = borrow(&env).to_core();
        for (pos, is_long) in [(long.as_ref(), true), (short.as_ref(), false)] {
            if let Some(p) = pos {
                let exec = exec_price(&c, &env, is_long, false, timestamp);
                collat += usd_of_token(&env, &c, &c.short_token, p.collateral_amount, timestamp);
                pnl += position_pnl_usd(p.size_usd, p.entry_price, exec, is_long).unwrap_or(0);
                fees += settle_fees(p, &f, &b).max(0);
                size += p.size_usd;
            }
        }
        if size == 0 || !account_is_liquidatable(collat, pnl, fees, size, mmr(&c)).unwrap_or(false)
        {
            panic_with_error!(&env, Error::NotLiquidatable);
        }
        let mut leftover = 0i128;
        if long.is_some() {
            leftover += liquidate_position(
                env.clone(),
                account.clone(),
                true,
                liquidator.clone(),
                liquidation_timestamp,
            );
        }
        if short.is_some() {
            leftover += liquidate_position(
                env.clone(),
                account.clone(),
                false,
                liquidator.clone(),
                liquidation_timestamp,
            );
        }
        let mut x = load_cross(&env, &account);
        if x.collateral_amount > 0 {
            PoolContractClient::new(&env, &c.pool).adjust_collateral(&-x.collateral_amount);
            PoolContractClient::new(&env, &c.pool).pay(
                &liquidator,
                &c.short_token,
                &(x.collateral_amount / 2),
            );
            x.collateral_amount = 0;
            save_cross(&env, &account, &x);
        }
        leftover
    }

    pub fn get_position(env: Env, account: Address, is_long: bool) -> Position {
        load_pos(&env, &account, is_long)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NoPosition))
    }

    pub fn get_oi(env: Env) -> OpenInterest {
        oi(&env)
    }

    pub fn get_funding(env: Env) -> FundingStore {
        funding(&env)
    }

    pub fn get_borrow(env: Env) -> BorrowStore {
        borrow(&env)
    }

    pub fn get_cross(env: Env, account: Address) -> CrossAccount {
        load_cross(&env, &account)
    }

    pub fn pending_pnl(env: Env) -> i128 {
        let timestamp = None;
        let c = cfg(&env);
        net_pending_pnl(&env, &c, timestamp)
    }

    pub fn config(env: Env) -> MarketConfig {
        cfg(&env)
    }

    pub fn is_liq(env: Env, account: Address, is_long: bool) -> bool {
        let timestamp = None;
        let c = cfg(&env);
        let p = match load_pos(&env, &account, is_long) {
            Some(p) => p,
            None => return false,
        };
        let exec = exec_price(&c, &env, is_long, false, timestamp);
        let fees = settle_fees(&p, &funding(&env).to_core(), &borrow(&env).to_core()).max(0);
        let pnl = position_pnl_usd(p.size_usd, p.entry_price, exec, is_long).unwrap_or(0);
        let extra = if p.isolated {
            0
        } else {
            let x = load_cross(&env, &account);
            usd_of_token(&env, &c, &c.short_token, x.collateral_amount, timestamp)
        };
        let collat = usd_of_token(&env, &c, &c.short_token, p.collateral_amount, timestamp) + extra;
        is_liquidatable(collat, pnl, fees, p.size_usd, mmr(&c)).unwrap_or(false)
    }
}

