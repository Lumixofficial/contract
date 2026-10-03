#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Address, Env};

#[test]
fn register_and_lookup() {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let market = Address::generate(&env);
    let pool = Address::generate(&env);
    let oracle = Address::generate(&env);
    let index = Address::generate(&env);
    let long = Address::generate(&env);
    let short = Address::generate(&env);
    let id = env.register(Factory, (admin,));
    let f = FactoryClient::new(&env, &id);
    f.register_market(&market, &pool, &oracle, &index, &long, &short);
    let info = f.market_for(&index);
    assert_eq!(info.market, market);
    assert_eq!(f.markets().len(), 1);
    assert!(f
        .try_register_market(&market, &pool, &oracle, &index, &long, &short)
        .is_err());
}

#[test]
fn disable_blocks_lookup() {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let index = Address::generate(&env);
    let id = env.register(Factory, (admin,));
    let f = FactoryClient::new(&env, &id);
    f.register_market(
        &Address::generate(&env),
        &Address::generate(&env),
        &Address::generate(&env),
        &index,
        &Address::generate(&env),
        &Address::generate(&env),
    );
    f.set_enabled(&index, &false);
    assert!(f.try_market_for(&index).is_err());
    assert!(!f.get_market(&index).enabled);
    assert!(!f.markets().get(0).unwrap().enabled);
    f.set_enabled(&index, &true);
    assert!(f.market_for(&index).enabled);
    assert!(f.markets().get(0).unwrap().enabled);
}
