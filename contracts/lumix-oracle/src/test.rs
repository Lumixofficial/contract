#![cfg(test)]

use super::*;
use lumix_core::PRICE_SCALE;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env,
};

fn setup() -> (Env, Address, Address, OracleClient<'static>) {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(Oracle, (admin.clone(), 600u64, 2_000u32, 2u32));
    let client = OracleClient::new(&env, &id);
    (env, admin, id, client)
}

#[test]
fn constructor_and_quote_fresh_price() {
    let (env, admin, _, client) = setup();
    let xlm = Address::generate(&env);
    client.set_mid(&admin, &xlm, &PRICE_SCALE);
    let p = client.quote(&xlm);
    assert!(p.min <= PRICE_SCALE && p.max >= PRICE_SCALE);
    assert_eq!(p.timestamp, env.ledger().timestamp());
}

#[test]
fn keeper_can_push_price() {
    let (env, _admin, _, client) = setup();
    let keeper = Address::generate(&env);
    let xlm = Address::generate(&env);
    client.set_keeper(&keeper, &true);
    client.set_price(&keeper, &xlm, &(PRICE_SCALE - 1), &(PRICE_SCALE + 1));
    let p = client.quote(&xlm);
    assert_eq!(p.min, PRICE_SCALE - 1);
}

fn assert_invoke_err<T, E>(r: Result<T, E>) {
    assert!(r.is_err());
}

#[test]
fn feeder_can_push_until_replaced() {
    let (env, admin, _, client) = setup();
    let feeder = Address::generate(&env);
    let keeper = Address::generate(&env);
    let xlm = Address::generate(&env);
    client.set_keeper(&keeper, &true);
    client.set_feeder(&feeder);
    assert_eq!(client.feeder(), Some(feeder.clone()));
    client.set_mid(&feeder, &xlm, &PRICE_SCALE);
    let pushed = client.quote(&xlm);
    assert!(pushed.min <= PRICE_SCALE && pushed.max >= PRICE_SCALE);

    client.set_mid(&keeper, &xlm, &(PRICE_SCALE + 10));
    client.set_mid(&admin, &xlm, &PRICE_SCALE);

    let replacement = Address::generate(&env);
    client.set_feeder(&replacement);
    assert_invoke_err(client.try_set_mid(&feeder, &xlm, &PRICE_SCALE));
    client.set_mid(&replacement, &xlm, &(PRICE_SCALE + 5));
    assert!(client.quote(&xlm).max >= PRICE_SCALE + 5);
}

#[test]
fn unauthorized_keeper_rejected() {
    let (env, _, _, client) = setup();
    let stranger = Address::generate(&env);
    let xlm = Address::generate(&env);
    assert_invoke_err(client.try_set_price(&stranger, &xlm, &PRICE_SCALE, &PRICE_SCALE));
}

#[test]
fn stale_price_rejected() {
    let (env, admin, _, client) = setup();
    let xlm = Address::generate(&env);
    client.set_mid(&admin, &xlm, &PRICE_SCALE);
    env.ledger().set_timestamp(env.ledger().timestamp() + 601);
    assert_invoke_err(client.try_quote(&xlm));
}

#[test]
fn deviation_guard() {
    let (env, admin, _, client) = setup();
    let xlm = Address::generate(&env);
    client.set_mid(&admin, &xlm, &PRICE_SCALE);
    env.ledger().set_timestamp(env.ledger().timestamp() + 1);
    let jumped = PRICE_SCALE * 3;
    assert_invoke_err(client.try_set_mid(&admin, &xlm, &jumped));
}

#[test]
fn off_hours_allows_older_quotes() {
    let (env, admin, _, client) = setup();
    let xlm = Address::generate(&env);
    client.set_mid(&admin, &xlm, &PRICE_SCALE);
    client.set_session(&false);
    env.ledger().set_timestamp(env.ledger().timestamp() + 700);
    // off-hours max age is 1200 in constructor (600*2)
    let p = client.quote(&xlm);
    assert!(p.min > 0);
}

#[test]
fn pause_blocks_quote_and_set() {
    let (env, admin, _, client) = setup();
    let xlm = Address::generate(&env);
    client.set_paused(&true);
    assert_invoke_err(client.try_set_mid(&admin, &xlm, &PRICE_SCALE));
}

#[test]
fn invalid_min_max_rejected() {
    let (env, admin, _, client) = setup();
    let xlm = Address::generate(&env);
    assert_invoke_err(client.try_set_price(&admin, &xlm, &2, &1));
    assert_invoke_err(client.try_set_price(&admin, &xlm, &0, &1));
}

#[test]
fn config_round_trip() {
    let (_, admin, _, client) = setup();
    client.set_params(&900u64, &1500u32, &3u32, &1800u64, &6u32);
    let cfg = client.config();
    assert_eq!(cfg.admin, admin);
    assert_eq!(cfg.max_age, 900);
    assert_eq!(cfg.spread_bps, 3);
    assert!(cfg.on_hours);
}

#[test]
fn first_write_wins_for_asset_and_timestamp_even_for_conflicting_retry() {
    let (env, admin, _, client) = setup();
    env.ledger().set_timestamp(1_000);
    let asset = Address::generate(&env);
    client.set_price_at(&admin, &asset, &100, &PRICE_SCALE, &(PRICE_SCALE + 10));
    let first = client.quote_at(&asset, &100);
    client.set_price_at(&admin, &asset, &100, &(PRICE_SCALE * 3), &(PRICE_SCALE * 3));
    client.set_price_at(&admin, &asset, &100, &0, &-1);
    client.set_mid_at(&admin, &asset, &100, &i128::MAX);
    assert_eq!(client.quote_at(&asset, &100), first);
    client.set_price_at(&admin, &asset, &200, &(PRICE_SCALE + 1), &(PRICE_SCALE + 1));
    client.set_price_at(&admin, &asset, &50, &(PRICE_SCALE / 2), &(PRICE_SCALE / 2));
    assert_eq!(client.quote_unchecked(&asset).timestamp, 200);
    assert_eq!(client.quote_at(&asset, &100), first);
    assert!(client.try_quote_at(&asset, &101).is_err());
}

#[test]
fn future_missing_invalid_and_paused_historical_quotes_fail() {
    let (env, admin, _, client) = setup();
    let asset = Address::generate(&env);
    env.ledger().set_timestamp(100);
    assert!(client
        .try_set_price_at(&admin, &asset, &101, &1, &1)
        .is_err());
    assert!(client.try_quote_at(&asset, &101).is_err());
    for (min, max) in [(0, 1), (-1, 1), (2, 1), (i128::MAX, i128::MAX)] {
        assert!(client
            .try_set_price_at(&admin, &asset, &100, &min, &max)
            .is_err());
    }
    assert!(client.try_quote_at(&asset, &100).is_err());
    client.set_price_at(&admin, &asset, &100, &PRICE_SCALE, &PRICE_SCALE);
    client.set_paused(&true);
    assert!(client.try_quote_at(&asset, &100).is_err());
    assert!(client
        .try_set_price_at(&admin, &asset, &100, &1, &1)
        .is_err());
    client.set_paused(&false);
    env.ledger().set_timestamp(1_000_000);
    assert_eq!(client.quote_at(&asset, &100).min, PRICE_SCALE);
    assert!(client.try_quote(&asset).is_err());
}

#[test]
fn fixed_quote_asset_is_explicit_permanent_and_not_retrofittable() {
    let (env, admin, _, client) = setup();
    let asset = Address::generate(&env);
    client.set_fixed_price(&asset, &PRICE_SCALE);
    env.ledger().set_timestamp(1_000_000);
    assert_eq!(client.quote_at(&asset, &900_000).min, PRICE_SCALE);
    assert_eq!(client.quote(&asset).timestamp, 1_000_000);
    client.set_price_at(&admin, &asset, &900_000, &1, &1);
    assert_eq!(client.quote_at(&asset, &900_000).min, PRICE_SCALE);
    assert!(client
        .try_set_fixed_price(&asset, &(2 * PRICE_SCALE))
        .is_err());
    let observed = Address::generate(&env);
    client.set_price_at(&admin, &observed, &100, &PRICE_SCALE, &PRICE_SCALE);
    assert!(client.try_set_fixed_price(&observed, &PRICE_SCALE).is_err());
}

#[test]
fn publisher_and_admin_need_actual_authorization() {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    let id = env.register(Oracle, (admin.clone(), 600u64, 2_000u32, 2u32));
    let c = OracleClient::new(&env, &id);
    assert!(c
        .try_set_price_at(&admin, &asset, &0, &PRICE_SCALE, &PRICE_SCALE)
        .is_err());
    assert!(c.try_set_source(&Some(asset.clone())).is_err());
    assert!(c.try_set_fixed_price(&asset, &PRICE_SCALE).is_err());
    assert!(c.try_set_feeder(&asset).is_err());
    assert!(c.try_set_keeper(&asset, &true).is_err());
}

#[soroban_sdk::contract]
struct Source;
#[soroban_sdk::contractimpl]
impl Source {
    pub fn price_at(env: Env, _asset: Address, timestamp: u64) -> lumix_interfaces::OraclePrice {
        let mode: u32 = env.storage().instance().get(&0u32).unwrap_or(0);
        lumix_interfaces::OraclePrice {
            min: if mode == 2 { 0 } else { PRICE_SCALE * 2 },
            max: PRICE_SCALE * 2,
            timestamp: if mode == 1 { timestamp + 1 } else { timestamp },
        }
    }
}

#[test]
fn source_resolves_exact_timestamp_caches_and_blocks_manual_override() {
    let (env, admin, _, client) = setup();
    env.ledger().set_timestamp(100);
    let asset = Address::generate(&env);
    let source = env.register(Source, ());
    client.set_source(&Some(source.clone()));
    assert!(client
        .try_set_price_at(&admin, &asset, &50, &PRICE_SCALE, &PRICE_SCALE)
        .is_err());
    assert_eq!(client.quote_at(&asset, &50).min, 2 * PRICE_SCALE);
    env.as_contract(&source, || env.storage().instance().set(&0u32, &1u32));
    assert_eq!(client.quote_at(&asset, &50).timestamp, 50);
    assert!(client.try_quote_at(&asset, &60).is_err());
    env.as_contract(&source, || env.storage().instance().set(&0u32, &2u32));
    assert!(client.try_quote_at(&asset, &60).is_err());
    client.set_source(&None);
    client.set_price_at(&admin, &asset, &60, &(2 * PRICE_SCALE), &(2 * PRICE_SCALE));
    assert_eq!(client.quote_at(&asset, &60).min, 2 * PRICE_SCALE);
    assert_eq!(client.quote_at(&asset, &50).min, 2 * PRICE_SCALE);
}

#[test]
fn history_uses_persistent_ttl_and_duplicate_writes_emit_no_price_update() {
    use soroban_sdk::testutils::{storage::Persistent, Events};
    let (env, admin, id, client) = setup();
    let asset = Address::generate(&env);
    client.set_price_at(&admin, &asset, &0, &PRICE_SCALE, &PRICE_SCALE);
    assert_eq!(env.events().all().events().len(), 1);
    client.set_price_at(&admin, &asset, &0, &(PRICE_SCALE * 2), &(PRICE_SCALE * 2));
    assert_eq!(env.events().all().events().len(), 0);
    env.as_contract(&id, || {
        assert!(
            env.storage()
                .persistent()
                .get_ttl(&DataKey::PriceAt(asset.clone(), 0))
                >= 3_999_999
        );
    });
}
