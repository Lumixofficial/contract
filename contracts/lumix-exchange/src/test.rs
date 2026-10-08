#![cfg(test)]

use super::*;
use lumix_core::{PRICE_SCALE, SCALAR_7};
use lumix_market::{Market, MarketClient};
use lumix_oracle::{Oracle, OracleClient};
use lumix_pool::{Pool, PoolClient};
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env, String,
};

struct Fx {
    env: Env,
    admin: Address,
    user: Address,
    keeper: Address,
    long: Address,
    short: Address,
    oracle: OracleClient<'static>,
    pool: PoolClient<'static>,
    market: MarketClient<'static>,
    exchange: ExchangeClient<'static>,
}

fn mint(env: &Env, sac: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, sac).mint(to, &amount);
}

fn setup() -> Fx {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let keeper = Address::generate(&env);
    let fee = Address::generate(&env);
    let long_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let short_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let long = long_sac.address();
    let short = short_sac.address();
    let oracle_id = env.register(Oracle, (admin.clone(), 600u64, 20_000u32, 0u32));
    let oracle = OracleClient::new(&env, &oracle_id);
    oracle.set_mid(&admin, &long, &(PRICE_SCALE / 2));
    oracle.set_mid(&admin, &short, &PRICE_SCALE);
    oracle.set_keeper(&keeper, &true);

    let pool_id = env.register(
        Pool,
        (
            admin.clone(),
            long.clone(),
            short.clone(),
            oracle_id.clone(),
            fee,
            String::from_str(&env, "GM"),
            String::from_str(&env, "GM"),
        ),
    );
    let pool = PoolClient::new(&env, &pool_id);
    let market_id = env.register(
        Market,
        (
            admin.clone(),
            pool_id.clone(),
            oracle_id.clone(),
            long.clone(),
            Address::generate(&env),
        ),
    );
    let market = MarketClient::new(&env, &market_id);
    pool.set_market(&market_id);

    let ex_id = env.register(Exchange, (admin.clone(), oracle_id.clone(), short.clone()));
    let exchange = ExchangeClient::new(&env, &ex_id);
    market.set_exchange(&ex_id);
    exchange.set_keeper(&keeper, &true);
    let factory_id = env.register(lumix_factory::Factory, (admin.clone(),));
    lumix_factory::FactoryClient::new(&env, &factory_id)
        .register_market(&market_id, &pool_id, &oracle_id, &long, &long, &short);
    exchange.set_factory(&factory_id);

    mint(&env, &long, &user, 1_000_000 * SCALAR_7);
    mint(&env, &short, &user, 1_000_000 * SCALAR_7);
    pool.deposit(&user, &(20_000 * SCALAR_7), &(10_000 * SCALAR_7), &0);

    Fx {
        env,
        admin,
        user,
        keeper,
        long,
        short,
        oracle,
        pool,
        market,
        exchange,
    }
}

#[test]
fn market_increase_then_decrease() {
    let f = setup();
    let id = f.exchange.create_order(
        &f.user,
        &f.market.address,
        &MARKET_INCREASE,
        &true,
        &(500 * SCALAR_7),
        &(2_000 * SCALAR_7),
        &PRICE_SCALE,
        &1,
        &(1 * SCALAR_7),
        &true,
        &0u64,
        &1u32,
    );
    f.exchange.execute_order(&f.keeper, &id);
    let p = f.market.get_position(&f.user, &true);
    assert_eq!(p.size_usd, 2_000 * SCALAR_7);

    let close = f.exchange.create_order(
        &f.user,
        &f.market.address,
        &MARKET_DECREASE,
        &true,
        &0,
        &(2_000 * SCALAR_7),
        &1,
        &1,
        &0,
        &true,
        &0u64,
        &1u32,
    );
    f.exchange.execute_order(&f.keeper, &close);
    assert!(f.market.try_get_position(&f.user, &true).is_err());
}

#[test]
fn limit_not_ready() {
    let f = setup();
    let id = f.exchange.create_order(
        &f.user,
        &f.market.address,
        &LIMIT_INCREASE,
        &true,
        &(100 * SCALAR_7),
        &(500 * SCALAR_7),
        &PRICE_SCALE,
        &(PRICE_SCALE / 10), // trigger far below current ~$0.50
        &0,
        &true,
        &0u64,
        &1u32,
    );
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
}

#[test]
fn cancel_refunds_fee() {
    let f = setup();
    let before = TokenClient::new(&f.env, &f.short).balance(&f.user);
    let id = f.exchange.create_order(
        &f.user,
        &f.market.address,
        &MARKET_INCREASE,
        &true,
        &(50 * SCALAR_7),
        &(200 * SCALAR_7),
        &PRICE_SCALE,
        &1,
        &(2 * SCALAR_7),
        &true,
        &0u64,
        &1u32,
    );
    f.exchange.cancel_order(&f.user, &id);
    let after = TokenClient::new(&f.env, &f.short).balance(&f.user);
    // Both pending collateral and the keeper fee are refunded.
    assert_eq!(after, before);
    assert!(f.exchange.get_order(&id).cancelled);
}

#[test]
fn twap_slices() {
    let f = setup();
    let id = f.exchange.create_order(
        &f.user,
        &f.market.address,
        &TWAP_INCREASE,
        &true,
        &(400 * SCALAR_7),
        &(800 * SCALAR_7),
        &PRICE_SCALE,
        &1,
        &0,
        &true,
        &0u64,
        &2u32,
    );
    f.exchange.execute_order(&f.keeper, &id);
    let p = f.market.get_position(&f.user, &true);
    assert_eq!(p.size_usd, 400 * SCALAR_7);
    f.exchange.execute_order(&f.keeper, &id);
    let p = f.market.get_position(&f.user, &true);
    assert_eq!(p.size_usd, 800 * SCALAR_7);
}
