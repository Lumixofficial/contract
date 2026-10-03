#![no_std]

use lumix_core::{INSTANCE_TTL_EXTEND_TO, INSTANCE_TTL_THRESHOLD};
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, vec,
    Address, Env, Vec,
};

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    Exists = 2,
    Missing = 3,
    Disabled = 4,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct MarketInfo {
    pub market: Address,
    pub pool: Address,
    pub oracle: Address,
    pub index_token: Address,
    pub long_token: Address,
    pub short_token: Address,
    pub enabled: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Markets,
    ByIndex(Address),
}

#[contractevent]
pub struct MarketRegistered {
    pub market: Address,
    pub pool: Address,
    pub index_token: Address,
}

#[contract]
pub struct Factory;

fn admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized))
}

fn list(env: &Env) -> Vec<MarketInfo> {
    env.storage()
        .instance()
        .get(&DataKey::Markets)
        .unwrap_or(vec![env])
}

#[contractimpl]
impl Factory {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Markets, &Vec::<MarketInfo>::new(&env));
        bump(&env);
    }

    pub fn register_market(
        env: Env,
        market: Address,
        pool: Address,
        oracle: Address,
        index_token: Address,
        long_token: Address,
        short_token: Address,
    ) {
        admin(&env).require_auth();
        if env
            .storage()
            .instance()
            .has(&DataKey::ByIndex(index_token.clone()))
        {
            panic_with_error!(&env, Error::Exists);
        }
        let info = MarketInfo {
            market: market.clone(),
            pool,
            oracle,
            index_token: index_token.clone(),
            long_token,
            short_token,
            enabled: true,
        };
        let mut m = list(&env);
        m.push_back(info.clone());
        env.storage().instance().set(&DataKey::Markets, &m);
        env.storage()
            .instance()
            .set(&DataKey::ByIndex(index_token.clone()), &info);
        MarketRegistered {
            market,
            pool: info.pool,
            index_token,
        }
        .publish(&env);
        bump(&env);
    }

    pub fn set_enabled(env: Env, index_token: Address, enabled: bool) {
        admin(&env).require_auth();
        let mut info: MarketInfo = env
            .storage()
            .instance()
            .get(&DataKey::ByIndex(index_token.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, Error::Missing));
        info.enabled = enabled;
        let mut markets = list(&env);
        for i in 0..markets.len() {
            if markets.get(i).unwrap().index_token == index_token {
                markets.set(i, info.clone());
                break;
            }
        }
        env.storage().instance().set(&DataKey::Markets, &markets);
        env.storage()
            .instance()
            .set(&DataKey::ByIndex(index_token), &info);
        bump(&env);
    }

    /// Registry lookup including disabled markets, so existing traders can
    /// still submit risk-reducing closes when new trading is disabled.
    pub fn get_market(env: Env, index_token: Address) -> MarketInfo {
        env.storage()
            .instance()
            .get(&DataKey::ByIndex(index_token))
            .unwrap_or_else(|| panic_with_error!(&env, Error::Missing))
    }

    pub fn market_for(env: Env, index_token: Address) -> MarketInfo {
        let info = Self::get_market(env.clone(), index_token);
        if !info.enabled {
            panic_with_error!(&env, Error::Disabled);
        }
        info
    }

    pub fn markets(env: Env) -> Vec<MarketInfo> {
        list(&env)
    }

    pub fn set_admin(env: Env, next: Address) {
        admin(&env).require_auth();
        env.storage().instance().set(&DataKey::Admin, &next);
        bump(&env);
    }
}

#[cfg(test)]
mod test;
