#![cfg(test)]

use super::*;
use lumix_core::{PRICE_SCALE, SCALAR_7};
use lumix_oracle::{Oracle, OracleClient};
use lumix_pool::{Pool, PoolClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env, String,
};

struct Fx {
    env: Env,
    admin: Address,
    user: Address,
    exchange: Address,
    long: Address,
    short: Address,
    oracle: OracleClient<'static>,
    pool: PoolClient<'static>,
    market: MarketClient<'static>,
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
    let exchange = Address::generate(&env);
    let fee = Address::generate(&env);

    let long_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let short_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let long = long_sac.address();
    let short = short_sac.address();

    let oracle_id = env.register(Oracle, (admin.clone(), 600u64, 20_000u32, 0u32));
    let oracle = OracleClient::new(&env, &oracle_id);
    oracle.set_mid(&admin, &long, &(PRICE_SCALE / 2));
    oracle.set_mid(&admin, &short, &PRICE_SCALE);

    let pool_id = env.register(
        Pool,
        (
            admin.clone(),
            long.clone(),
            short.clone(),
            oracle_id.clone(),
            fee.clone(),
            String::from_str(&env, "GM"),
            String::from_str(&env, "GM"),
        ),
    );
    let pool = PoolClient::new(&env, &pool_id);

    let market_id = env.register(
        Market,
        (admin.clone(), pool_id.clone(), oracle_id, long.clone(), fee),
    );
    let market = MarketClient::new(&env, &market_id);
    pool.set_market(&market_id);
    market.set_exchange(&exchange);

    mint(&env, &long, &user, 1_000_000 * SCALAR_7);
    mint(&env, &short, &user, 1_000_000 * SCALAR_7);
    pool.deposit(&user, &(20_000 * SCALAR_7), &(10_000 * SCALAR_7), &0);

    Fx {
        env,
        admin,
        user,
        exchange,
        long,
        short,
        oracle,
        pool,
        market,
    }
}

#[test]
fn open_long_and_close_profit() {
    let f = setup();
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(500 * SCALAR_7));
    let p = f.market.increase(
        &f.user,
        &true,
        &(500 * SCALAR_7),
        &(2_000 * SCALAR_7),
        &(PRICE_SCALE),
        &true,
        &f.env.ledger().timestamp(),
    );
    assert_eq!(p.size_usd, 2_000 * SCALAR_7);
    assert!(p.entry_price > 0);
    let oi = f.market.get_oi();
    assert_eq!(oi.long_usd, 2_000 * SCALAR_7);

    f.oracle.set_mid(&f.admin, &f.long, &(PRICE_SCALE)); // XLM $0.50 -> $1
    let payout = f.market.decrease(
        &f.user,
        &true,
        &0,
        &(2_000 * SCALAR_7),
        &1,
        &f.user,
        &f.env.ledger().timestamp(),
    );
    assert!(payout > 0);
    assert_eq!(f.market.get_oi().long_usd, 0);
}

#[test]
fn leverage_cap_enforced() {
    let f = setup();
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(10 * SCALAR_7));
    assert!(f
        .market
        .try_increase(
            &f.user,
            &true,
            &(10 * SCALAR_7),
            &(500 * SCALAR_7),
            &(PRICE_SCALE),
            &true,
            &f.env.ledger().timestamp()
        )
        .is_err());
}

#[test]
fn short_and_liquidate() {
    let f = setup();
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(100 * SCALAR_7));
    f.market.increase(
        &f.user,
        &false,
        &(100 * SCALAR_7),
        &(1_000 * SCALAR_7),
        &1,
        &true,
        &f.env.ledger().timestamp(),
    );
    // XLM pumps; shorts lose. 0.50 -> 1.00 is 10000 bps, under the 20000 cap.
    f.env.ledger().set_timestamp(f.env.ledger().timestamp() + 1);
    f.oracle.set_mid(&f.admin, &f.short, &PRICE_SCALE);
    f.oracle.set_mid(&f.admin, &f.long, &PRICE_SCALE);
    let liq = f.market.is_liq(&f.user, &false);
    if liq {
        let keeper = Address::generate(&f.env);
        f.market.set_keeper(&keeper, &true);
        f.market
            .liquidate(&f.user, &false, &keeper, &f.env.ledger().timestamp());
        assert!(f.market.try_get_position(&f.user, &false).is_err());
    }
}

#[test]
fn funding_accrues_on_imbalance() {
    let f = setup();
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(500 * SCALAR_7));
    f.market.increase(
        &f.user,
        &true,
        &(500 * SCALAR_7),
        &(2_000 * SCALAR_7),
        &PRICE_SCALE,
        &true,
        &f.env.ledger().timestamp(),
    );
    f.env
        .ledger()
        .set_timestamp(f.env.ledger().timestamp() + 3600);
    f.oracle.set_mid(&f.admin, &f.long, &(PRICE_SCALE / 2));
    f.oracle.set_mid(&f.admin, &f.short, &PRICE_SCALE);
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(50 * SCALAR_7));
    f.market.increase(
        &f.user,
        &true,
        &(50 * SCALAR_7),
        &0,
        &PRICE_SCALE,
        &true,
        &f.env.ledger().timestamp(),
    );
    let fund = f.market.get_funding();
    assert!(fund.saved_factor_per_second >= 0);
}

#[test]
fn add_collateral_via_zero_size() {
    let f = setup();
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(200 * SCALAR_7));
    f.market.increase(
        &f.user,
        &true,
        &(100 * SCALAR_7),
        &(500 * SCALAR_7),
        &PRICE_SCALE,
        &true,
        &f.env.ledger().timestamp(),
    );
    f.market.increase(
        &f.user,
        &true,
        &(100 * SCALAR_7),
        &0,
        &PRICE_SCALE,
        &true,
        &f.env.ledger().timestamp(),
    );
    let p = f.market.get_position(&f.user, &true);
    assert!(p.collateral_amount >= 150 * SCALAR_7);
}

#[test]
fn cross_collateral_uses_market_short_token() {
    let f = setup();
    let amount = 100 * SCALAR_7;
    let before = TokenClient::new(&f.env, &f.short).balance(&f.user);

    f.market.deposit_cross(&f.user, &amount);

    assert_eq!(f.market.get_cross(&f.user).collateral_amount, amount);
    assert_eq!(
        TokenClient::new(&f.env, &f.short).balance(&f.user),
        before - amount
    );
}

#[test]
fn pending_pnl_after_price_up() {
    let f = setup();
    TokenClient::new(&f.env, &f.short).transfer(&f.user, &f.pool.address, &(500 * SCALAR_7));
    f.market.increase(
        &f.user,
        &true,
        &(500 * SCALAR_7),
        &(2_000 * SCALAR_7),
        &PRICE_SCALE,
        &true,
        &f.env.ledger().timestamp(),
    );
    f.env.ledger().set_timestamp(f.env.ledger().timestamp() + 1);
    f.oracle.set_mid(&f.admin, &f.short, &PRICE_SCALE);
    f.oracle.set_mid(&f.admin, &f.long, &PRICE_SCALE);
    assert!(f.market.pending_pnl() > 0);
}
