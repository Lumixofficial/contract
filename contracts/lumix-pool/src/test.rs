#![cfg(test)]

use super::*;
use lumix_core::{MINIMUM_LIQUIDITY, PRICE_SCALE, SCALAR_7};
use lumix_oracle::OracleClient;
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env, String,
};

struct Fixture {
    env: Env,
    admin: Address,
    user: Address,
    _market: Address,
    long: Address,
    short: Address,
    _oracle: OracleClient<'static>,
    pool: PoolClient<'static>,
    long_token: TokenClient<'static>,
    short_token: TokenClient<'static>,
}

fn mint(env: &Env, sac: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, sac).mint(to, &amount);
}

fn setup() -> Fixture {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let market = Address::generate(&env);
    let fee = Address::generate(&env);

    let long_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let short_sac = env.register_stellar_asset_contract_v2(admin.clone());
    let long = long_sac.address();
    let short = short_sac.address();

    let oracle_id = env.register(
        lumix_oracle::Oracle,
        (admin.clone(), 600u64, 20_000u32, 0u32),
    );
    let oracle = OracleClient::new(&env, &oracle_id);
    oracle.set_mid(&admin, &long, &(PRICE_SCALE / 2)); // $0.50
    oracle.set_mid(&admin, &short, &PRICE_SCALE); // $1

    let pool_id = env.register(
        Pool,
        (
            admin.clone(),
            long.clone(),
            short.clone(),
            oracle_id,
            fee,
            String::from_str(&env, "GM XLM-USDC"),
            String::from_str(&env, "GM"),
        ),
    );
    let pool = PoolClient::new(&env, &pool_id);
    pool.set_market(&market);

    mint(&env, &long, &user, 1_000_000 * SCALAR_7);
    mint(&env, &short, &user, 1_000_000 * SCALAR_7);

    let long_token = TokenClient::new(&env, &long);
    let short_token = TokenClient::new(&env, &short);

    Fixture {
        env,
        admin,
        user,
        _market: market,
        long,
        short,
        _oracle: oracle,
        pool,
        long_token,
        short_token,
    }
}

#[test]
fn pair_deposit_and_withdraw() {
    let f = setup();
    let long_in = 200 * SCALAR_7; // $100
    let short_in = 100 * SCALAR_7; // $100
    let shares = f.pool.deposit(&f.user, &long_in, &short_in, &0);
    assert!(shares > 0);
    assert_eq!(f.pool.balance(&f.user), shares);
    assert_eq!(f.pool.total_supply(), shares + MINIMUM_LIQUIDITY);

    let (l, s) = f.pool.withdraw(&f.user, &shares, &0, &0);
    assert!(l <= long_in);
    assert!(s <= short_in);
    assert_eq!(f.pool.balance(&f.user), 0);
}

#[test]
fn first_deposit_locks_minimum_liquidity() {
    let f = setup();
    f.pool
        .deposit(&f.user, &(20 * SCALAR_7), &(10 * SCALAR_7), &0);
    assert_eq!(f.pool.balance(&f.pool.address), MINIMUM_LIQUIDITY);
}

#[test]
fn withdraw_blocked_when_reserved() {
    let f = setup();
    f.pool
        .deposit(&f.user, &(2_000 * SCALAR_7), &(1_000 * SCALAR_7), &0);
    // reserve nearly all XLM
    f.pool.reserve(&true, &(1_700 * SCALAR_7));
    let shares = f.pool.balance(&f.user);
    assert!(f.pool.try_withdraw(&f.user, &shares, &0, &0).is_err());
}

#[test]
fn swap_oracle_priced() {
    let f = setup();
    f.pool
        .deposit(&f.user, &(2_000 * SCALAR_7), &(1_000 * SCALAR_7), &0);
    let out = f.pool.swap(&f.user, &f.short, &(10 * SCALAR_7), &0, &false);
    assert!(out > 0);
    // ~$10 of USDC -> XLM at $0.50 ≈ 20 XLM minus fees
    assert!(out < 20 * SCALAR_7);
    assert!(out > 15 * SCALAR_7);
}

#[test]
fn atomic_swap_more_expensive() {
    let f = setup();
    f.pool
        .deposit(&f.user, &(4_000 * SCALAR_7), &(2_000 * SCALAR_7), &0);
    let normal = f.pool.swap(&f.user, &f.short, &(10 * SCALAR_7), &0, &false);
    let atomic = f.pool.swap(&f.user, &f.short, &(10 * SCALAR_7), &0, &true);
    assert!(atomic < normal);
}

#[test]
fn share_transfer() {
    let f = setup();
    let shares = f
        .pool
        .deposit(&f.user, &(20 * SCALAR_7), &(10 * SCALAR_7), &0);
    let other = Address::generate(&f.env);
    f.pool.transfer(&f.user, &other, &(shares / 2));
    assert_eq!(f.pool.balance(&other), shares / 2);
}

#[test]
fn pause_blocks_deposit() {
    let f = setup();
    f.pool.set_status(&true, &true, &true);
    assert!(f
        .pool
        .try_deposit(&f.user, &SCALAR_7, &SCALAR_7, &0)
        .is_err());
}

#[test]
fn max_pool_cap() {
    let f = setup();
    f.pool
        .set_params(&8_500u32, &2_000u32, &(50 * SCALAR_7), &f.admin);
    assert!(f
        .pool
        .try_deposit(&f.user, &(200 * SCALAR_7), &(100 * SCALAR_7), &0)
        .is_err());
}

#[test]
fn share_price_rises_after_fees_in_pool() {
    let f = setup();
    f.pool
        .deposit(&f.user, &(200 * SCALAR_7), &(100 * SCALAR_7), &0);
    let before = f.pool.share_price();
    // donate USDC as if trading fees accrued
    mint(&f.env, &f.short, &f.pool.address, 50 * SCALAR_7);
    let after = f.pool.share_price();
    assert!(after > before);
}

#[test]
fn slippage_on_deposit() {
    let f = setup();
    assert!(f
        .pool
        .try_deposit(&f.user, &(20 * SCALAR_7), &(10 * SCALAR_7), &i128::MAX)
        .is_err());
}

#[test]
fn release_allows_withdraw() {
    let f = setup();
    f.pool
        .deposit(&f.user, &(200 * SCALAR_7), &(100 * SCALAR_7), &0);
    f.pool.reserve(&true, &(100 * SCALAR_7));
    f.pool.release(&true, &(100 * SCALAR_7));
    let shares = f.pool.balance(&f.user);
    let (l, s) = f.pool.withdraw(&f.user, &shares, &0, &0);
    assert!(l > 0 && s > 0);
}
