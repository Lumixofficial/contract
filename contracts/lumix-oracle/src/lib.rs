#![no_std]

use lumix_core::{
    deviation_bps, mid_price, min_max_from_mid, DEFAULT_ORACLE_MAX_AGE,
    DEFAULT_ORACLE_MAX_DEVIATION_BPS, DEFAULT_ORACLE_SPREAD_BPS, INSTANCE_TTL_EXTEND_TO,
    INSTANCE_TTL_THRESHOLD, PERSISTENT_TTL_THRESHOLD,
};
use lumix_interfaces::PriceSourceClient;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, Address,
    Env,
};

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn bump_key(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERSISTENT_TTL_THRESHOLD, 4_000_000);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    InvalidPrice = 2,
    StalePrice = 3,
    ExcessiveDeviation = 4,
    Paused = 5,
    MissingPrice = 6,
    InvalidConfig = 7,
    FuturePrice = 8,
    SourceEnabled = 9,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Price {
    pub min: i128,
    pub max: i128,
    pub timestamp: u64,
}

impl Price {
    pub fn mid(&self) -> i128 {
        mid_price(self.min, self.max).unwrap_or(self.min)
    }
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Config {
    pub admin: Address,
    pub max_age: u64,
    pub max_deviation_bps: u32,
    pub spread_bps: u32,
    pub on_hours: bool,
    pub off_hours_max_age: u64,
    pub off_hours_spread_bps: u32,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    Price(Address),
    Keeper(Address),
    Feed,
    PriceAt(Address, u64),
    Source,
    Fixed(Address),
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceUpdated {
    pub asset: Address,
    pub min: i128,
    pub max: i128,
    pub timestamp: u64,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionChanged {
    pub on_hours: bool,
}

#[contract]
pub struct Oracle;

fn config(env: &Env) -> Config {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::InvalidConfig))
}

fn require_admin(_env: &Env, cfg: &Config) {
    cfg.admin.require_auth();
}

fn require_price_publisher(env: &Env, caller: &Address, cfg: &Config) {
    caller.require_auth();
    if *caller == cfg.admin {
        return;
    }
    if let Some(feeder) = env.storage().instance().get::<_, Address>(&DataKey::Feed) {
        if feeder == *caller {
            return;
        }
    }
    let allowed: bool = env
        .storage()
        .instance()
        .get(&DataKey::Keeper(caller.clone()))
        .unwrap_or(false);
    if !allowed {
        panic_with_error!(env, Error::Unauthorized);
    }
}

fn effective_max_age(cfg: &Config) -> u64 {
    if cfg.on_hours {
        cfg.max_age
    } else {
        cfg.off_hours_max_age
    }
}

fn effective_spread(cfg: &Config) -> u32 {
    if cfg.on_hours {
        cfg.spread_bps
    } else {
        cfg.off_hours_spread_bps
    }
}

fn check_time(env: &Env, cfg: &Config, timestamp: u64) {
    if cfg.paused {
        panic_with_error!(env, Error::Paused);
    }
    if timestamp > env.ledger().timestamp() {
        panic_with_error!(env, Error::FuturePrice);
    }
}

fn stored_price(env: &Env, asset: &Address, timestamp: u64) -> Option<Price> {
    for key in [
        DataKey::PriceAt(asset.clone(), timestamp),
        DataKey::Fixed(asset.clone()),
    ] {
        if let Some(mut price) = env.storage().persistent().get::<_, Price>(&key) {
            bump_key(env, &key);
            price.timestamp = timestamp;
            return Some(price);
        }
    }
    None
}

fn save_price(env: &Env, cfg: &Config, asset: Address, price: Price) {
    if price.min <= 0 || price.max < price.min || mid_price(price.min, price.max).is_err() {
        panic_with_error!(env, Error::InvalidPrice);
    }
    let latest = DataKey::Price(asset.clone());
    let prev = env.storage().persistent().get::<_, Price>(&latest);
    // Compare nearby forward observations only. Historical backfills and long
    // gaps must not require fabricated intermediate prices to pass this guard.
    if let Some(ref previous) = prev {
        if price.timestamp > previous.timestamp
            && price.timestamp - previous.timestamp <= effective_max_age(cfg)
        {
            let dev = deviation_bps(price.mid(), previous.mid())
                .unwrap_or_else(|_| panic_with_error!(env, Error::InvalidPrice));
            if dev > i128::from(cfg.max_deviation_bps) {
                panic_with_error!(env, Error::ExcessiveDeviation);
            }
        }
    }
    let key = DataKey::PriceAt(asset.clone(), price.timestamp);
    env.storage().persistent().set(&key, &price);
    bump_key(env, &key);
    if prev.map(|p| price.timestamp > p.timestamp).unwrap_or(true) {
        env.storage().persistent().set(&latest, &price);
        bump_key(env, &latest);
    }
    PriceUpdated {
        asset,
        min: price.min,
        max: price.max,
        timestamp: price.timestamp,
    }
    .publish(env);
    bump(env);
}

#[contractimpl]
impl Oracle {
    pub fn __constructor(
        env: Env,
        admin: Address,
        max_age: u64,
        max_deviation_bps: u32,
        spread_bps: u32,
    ) {
        if max_age == 0 || spread_bps > 1_000 {
            panic_with_error!(&env, Error::InvalidConfig);
        }
        let cfg = Config {
            admin,
            max_age: if max_age == 0 {
                DEFAULT_ORACLE_MAX_AGE
            } else {
                max_age
            },
            max_deviation_bps: if max_deviation_bps == 0 {
                DEFAULT_ORACLE_MAX_DEVIATION_BPS
            } else {
                max_deviation_bps
            },
            spread_bps: if spread_bps == 0 {
                DEFAULT_ORACLE_SPREAD_BPS
            } else {
                spread_bps
            },
            on_hours: true,
            off_hours_max_age: max_age.max(DEFAULT_ORACLE_MAX_AGE).saturating_mul(2),
            off_hours_spread_bps: spread_bps.saturating_mul(2).max(DEFAULT_ORACLE_SPREAD_BPS),
            paused: false,
        };
        env.storage().instance().set(&DataKey::Config, &cfg);
        bump(&env);
    }

    pub fn set_keeper(env: Env, keeper: Address, allowed: bool) {
        let cfg = config(&env);
        require_admin(&env, &cfg);
        env.storage()
            .instance()
            .set(&DataKey::Keeper(keeper), &allowed);
        bump(&env);
    }

    /// One account allowed to push prices, separate from order-execution keepers.
    /// Temporary stand-in until the oracle reads a market source itself.
    pub fn set_feeder(env: Env, feeder: Address) {
        let cfg = config(&env);
        require_admin(&env, &cfg);
        env.storage().instance().set(&DataKey::Feed, &feeder);
        bump(&env);
    }

    pub fn set_paused(env: Env, paused: bool) {
        let mut cfg = config(&env);
        require_admin(&env, &cfg);
        cfg.paused = paused;
        env.storage().instance().set(&DataKey::Config, &cfg);
        bump(&env);
    }

    pub fn set_session(env: Env, on_hours: bool) {
        let mut cfg = config(&env);
        require_admin(&env, &cfg);
        cfg.on_hours = on_hours;
        env.storage().instance().set(&DataKey::Config, &cfg);
        SessionChanged { on_hours }.publish(&env);
        bump(&env);
    }

    pub fn set_params(
        env: Env,
        max_age: u64,
        max_deviation_bps: u32,
        spread_bps: u32,
        off_hours_max_age: u64,
        off_hours_spread_bps: u32,
    ) {
        let mut cfg = config(&env);
        require_admin(&env, &cfg);
        if max_age == 0
            || off_hours_max_age == 0
            || spread_bps > 1_000
            || off_hours_spread_bps > 1_000
        {
            panic_with_error!(&env, Error::InvalidConfig);
        }
        cfg.max_age = max_age;
        cfg.max_deviation_bps = max_deviation_bps;
        cfg.spread_bps = spread_bps;
        cfg.off_hours_max_age = off_hours_max_age;
        cfg.off_hours_spread_bps = off_hours_spread_bps;
        env.storage().instance().set(&DataKey::Config, &cfg);
        bump(&env);
    }

    /// First successful publication wins, including conflicting retries.
    pub fn set_price_at(
        env: Env,
        caller: Address,
        asset: Address,
        timestamp: u64,
        min: i128,
        max: i128,
    ) {
        let cfg = config(&env);
        require_price_publisher(&env, &caller, &cfg);
        check_time(&env, &cfg, timestamp);
        if stored_price(&env, &asset, timestamp).is_some() {
            return;
        }
        if env.storage().instance().has(&DataKey::Source) {
            panic_with_error!(&env, Error::SourceEnabled);
        }
        save_price(
            &env,
            &cfg,
            asset,
            Price {
                min,
                max,
                timestamp,
            },
        );
    }

    /// Convenience publication for the current ledger timestamp.
    pub fn set_price(env: Env, caller: Address, asset: Address, min: i128, max: i128) {
        let timestamp = env.ledger().timestamp();
        Self::set_price_at(env, caller, asset, timestamp, min, max);
    }

    pub fn set_mid(env: Env, caller: Address, asset: Address, mid: i128) {
        let timestamp = env.ledger().timestamp();
        Self::set_mid_at(env, caller, asset, timestamp, mid);
    }

    pub fn set_mid_at(env: Env, caller: Address, asset: Address, timestamp: u64, mid: i128) {
        let cfg = config(&env);
        require_price_publisher(&env, &caller, &cfg);
        check_time(&env, &cfg, timestamp);
        if stored_price(&env, &asset, timestamp).is_some() {
            return;
        }
        let (min, max) = min_max_from_mid(mid, effective_spread(&cfg))
            .unwrap_or_else(|_| panic_with_error!(&env, Error::InvalidPrice));
        if env.storage().instance().has(&DataKey::Source) {
            panic_with_error!(&env, Error::SourceEnabled);
        }
        save_price(
            &env,
            &cfg,
            asset,
            Price {
                min,
                max,
                timestamp,
            },
        );
    }

    /// Optional on-chain adapter. It must return a quote for the exact requested
    /// second in the 14-decimal USD scale. Existing observations never change.
    pub fn set_source(env: Env, source: Option<Address>) {
        let cfg = config(&env);
        require_admin(&env, &cfg);
        if let Some(address) = source {
            if address == env.current_contract_address() {
                panic_with_error!(&env, Error::InvalidConfig);
            }
            env.storage().instance().set(&DataKey::Source, &address);
        } else {
            env.storage().instance().remove(&DataKey::Source);
        }
        bump(&env);
    }

    pub fn source(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Source)
    }

    /// Explicit, permanent accounting peg, e.g. USDC = $1. Only configurable
    /// before this asset has observations. This is not a market price feed.
    pub fn set_fixed_price(env: Env, asset: Address, price: i128) {
        let cfg = config(&env);
        require_admin(&env, &cfg);
        if price <= 0
            || mid_price(price, price).is_err()
            || env
                .storage()
                .persistent()
                .has(&DataKey::Price(asset.clone()))
            || env
                .storage()
                .persistent()
                .has(&DataKey::Fixed(asset.clone()))
        {
            panic_with_error!(&env, Error::InvalidConfig);
        }
        let key = DataKey::Fixed(asset.clone());
        let quote = Price {
            min: price,
            max: price,
            timestamp: 0,
        };
        env.storage().persistent().set(&key, &quote);
        bump_key(&env, &key);
        PriceUpdated {
            asset,
            min: price,
            max: price,
            timestamp: 0,
        }
        .publish(&env);
        bump(&env);
    }

    /// Resolve an exact historical observation. No latest-price fallback and no
    /// age limit: a delayed request still uses its original timestamp.
    pub fn quote_at(env: Env, asset: Address, timestamp: u64) -> Price {
        let cfg = config(&env);
        check_time(&env, &cfg, timestamp);
        if let Some(price) = stored_price(&env, &asset, timestamp) {
            bump(&env);
            return price;
        }
        let source: Address = env
            .storage()
            .instance()
            .get(&DataKey::Source)
            .unwrap_or_else(|| panic_with_error!(&env, Error::MissingPrice));
        let fetched = PriceSourceClient::new(&env, &source).price_at(&asset, &timestamp);
        if fetched.timestamp != timestamp {
            panic_with_error!(&env, Error::InvalidPrice);
        }
        let price = Price {
            min: fetched.min,
            max: fetched.max,
            timestamp,
        };
        save_price(&env, &cfg, asset, price.clone());
        price
    }

    /// Latest observed price, for live LP operations and views only.
    pub fn quote(env: Env, asset: Address) -> Price {
        let cfg = config(&env);
        check_time(&env, &cfg, env.ledger().timestamp());
        let fixed = DataKey::Fixed(asset.clone());
        if let Some(mut price) = env.storage().persistent().get::<_, Price>(&fixed) {
            price.timestamp = env.ledger().timestamp();
            bump_key(&env, &fixed);
            bump(&env);
            return price;
        }
        let price = Self::quote_unchecked(env.clone(), asset);
        if price.timestamp > env.ledger().timestamp()
            || env.ledger().timestamp() - price.timestamp > effective_max_age(&cfg)
        {
            panic_with_error!(&env, Error::StalePrice);
        }
        bump(&env);
        price
    }

    pub fn quote_unchecked(env: Env, asset: Address) -> Price {
        let fixed = DataKey::Fixed(asset.clone());
        let key = if env.storage().persistent().has(&fixed) {
            fixed
        } else {
            DataKey::Price(asset)
        };
        let price = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::MissingPrice));
        bump_key(&env, &key);
        bump(&env);
        price
    }

    pub fn config(env: Env) -> Config {
        config(&env)
    }

    pub fn is_keeper(env: Env, keeper: Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Keeper(keeper))
            .unwrap_or(false)
    }

    pub fn feeder(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Feed)
    }
}

#[cfg(test)]
mod test;
