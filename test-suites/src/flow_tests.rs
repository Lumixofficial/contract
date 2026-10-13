use crate::harness::*;
use lumix_core::{PRICE_SCALE, SCALAR_7};
use lumix_exchange::{MARKET_DECREASE, MARKET_INCREASE, TWAP_INCREASE};
use lumix_options::{OptionKind, OrderSide, OrderStatus, PositionStatus};
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, IntoVal,
};

#[test]
fn all_products_and_pool_use_the_registered_oracle() {
    let f = Fixture::default();
    let id = f.oracle.address.clone();
    assert_eq!(f.market.config().oracle, id);
    assert_eq!(f.exchange.config().oracle, id);
    assert_eq!(f.options.config().oracle, id);
    assert_eq!(f.pool.oracle(), id);
    assert_eq!(f.factory.market_for(&f.base).oracle, id);
}

#[test]
fn delayed_open_and_close_use_request_prices_not_latest_quotes() {
    let f = Fixture::default();
    let user_before = f.balance(&f.quote, &f.alice);
    let pool_before = f.balance(&f.quote, &f.pool.address);
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        2_000 * SCALAR_7,
        SCALAR_7,
    );
    assert_eq!(f.exchange.get_order(&id).created_at, START + 1);
    assert_eq!(f.balance(&f.quote, &f.alice), user_before - 501 * SCALAR_7);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 501 * SCALAR_7);
    assert_eq!(f.balance(&f.quote, &f.pool.address), pool_before);
    assert!(f.market.try_get_position(&f.alice, &true).is_err());
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.time(START + 4_000);
    f.publish(START + 4_000, 2 * PRICE_SCALE);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.publish(START + 1, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    let p = f.market.get_position(&f.alice, &true);
    assert_eq!(p.opened_at, START + 1);
    assert!(p.entry_price >= PRICE_SCALE && p.entry_price < PRICE_SCALE * 101 / 100);
    assert_eq!(f.pool.collateral(), p.collateral_amount);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 0);
    assert_eq!(f.balance(&f.quote, &f.keeper), SCALAR_7);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    assert!(f.exchange.try_cancel_order(&f.alice, &id).is_err());
    f.time(START + 4_001);
    let close = f.order(&f.alice, MARKET_DECREASE, true, 0, p.size_usd, 0);
    assert_eq!(f.market.get_position(&f.alice, &true), p);
    assert!(f.exchange.try_execute_order(&f.keeper, &close).is_err());
    f.time(START + 8_000);
    f.publish(START + 8_000, 3 * PRICE_SCALE);
    f.publish(START + 4_001, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &close);
    assert!(f.market.try_get_position(&f.alice, &true).is_err());
    assert_eq!(f.pool.collateral(), 0);
    assert_eq!(f.market.get_oi().long_usd, 0);
    assert!(f.balance(&f.quote, &f.alice) < user_before);
    f.assert_conservation();
}

#[test]
fn simultaneous_perps_and_options_share_first_observation() {
    let f = Fixture::default();
    let a = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        2_000 * SCALAR_7,
        0,
    );
    let b = f.order(
        &f.bob,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        2_000 * SCALAR_7,
        0,
    );
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        OptionKind::Call,
        10 * SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = f.option(
        &f.bob,
        OrderSide::Sell,
        OptionKind::Call,
        10 * SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    f.time(START + 50);
    f.publish(START + 1, PRICE_SCALE);
    f.publish(START + 1, 2 * PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &b);
    f.exchange.execute_order(&f.keeper, &a);
    let opt = f.options.match_orders(&f.keeper, &buy, &sell);
    assert_eq!(f.options.get_position(&opt).buyer_price, PRICE_SCALE);
    assert_eq!(f.options.get_position(&opt).seller_price, PRICE_SCALE);
    for trader in [&f.alice, &f.bob] {
        assert!(f.market.get_position(trader, &true).entry_price < PRICE_SCALE * 101 / 100);
    }
    f.assert_conservation();
}

#[test]
fn cancellation_refunds_all_escrow_even_when_frozen_or_oracle_paused() {
    let f = Fixture::default();
    let before = f.balance(&f.quote, &f.alice);
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        false,
        100 * SCALAR_7,
        500 * SCALAR_7,
        SCALAR_7,
    );
    f.exchange.freeze_order(&id);
    f.oracle.set_paused(&true);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.exchange.cancel_order(&f.alice, &id);
    assert_eq!(f.balance(&f.quote, &f.alice), before);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 0);
    assert!(f.exchange.try_cancel_order(&f.alice, &id).is_err());
    f.assert_conservation();
}

#[test]
fn failed_slippage_is_atomic_and_request_can_be_cancelled() {
    let f = Fixture::default();
    let id = f.exchange.create_order(
        &f.alice,
        &f.market.address,
        &MARKET_INCREASE,
        &true,
        &(100 * SCALAR_7),
        &(500 * SCALAR_7),
        &(PRICE_SCALE / 2),
        &0,
        &SCALAR_7,
        &true,
        &0,
        &1,
    );
    f.publish(START + 1, PRICE_SCALE);
    let pool_before = f.balance(&f.quote, &f.pool.address);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    assert_eq!(f.balance(&f.quote, &f.pool.address), pool_before);
    assert_eq!(f.pool.collateral(), 0);
    assert_eq!(f.pool.get_state().reserved_long, 0);
    assert_eq!(f.market.get_oi().long_usd, 0);
    assert_eq!(f.balance(&f.quote, &f.keeper), 0);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 101 * SCALAR_7);
    assert!(!f.exchange.get_order(&id).executed);
    f.exchange.cancel_order(&f.alice, &id);
    f.assert_conservation();
}

#[test]
fn liquidation_is_separate_from_pending_close_and_requires_keeper_and_recent_price() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        100 * SCALAR_7,
        1_000 * SCALAR_7,
        0,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    f.time(START + 2);
    let close = f.order(
        &f.alice,
        MARKET_DECREASE,
        true,
        0,
        1_000 * SCALAR_7,
        SCALAR_7,
    );
    f.time(START + 3);
    assert!(f
        .market
        .try_liquidate(&f.alice, &true, &f.keeper, &(START + 3))
        .is_err());
    f.publish(START + 3, PRICE_SCALE / 2);
    assert!(f
        .market
        .try_liquidate(&f.alice, &true, &f.bob, &(START + 3))
        .is_err());
    assert!(f
        .market
        .try_liquidate(&f.alice, &true, &f.keeper, &(START + 4))
        .is_err());
    f.time(START + 604);
    assert!(f
        .market
        .try_liquidate(&f.alice, &true, &f.keeper, &(START + 3))
        .is_err());
    f.publish(START + 604, PRICE_SCALE / 2);
    f.market
        .liquidate(&f.alice, &true, &f.keeper, &(START + 604));
    assert!(f.market.try_get_position(&f.alice, &true).is_err());
    assert_eq!(f.pool.collateral(), 0);
    assert_eq!(f.market.get_oi().long_usd, 0);
    f.publish(START + 2, PRICE_SCALE);
    assert!(f.exchange.try_execute_order(&f.keeper, &close).is_err());
    assert_eq!(f.balance(&f.quote, &f.exchange.address), SCALAR_7);
    f.exchange.cancel_order(&f.alice, &close);
    f.assert_conservation();
}

#[test]
fn healthy_position_cannot_be_liquidated() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        false,
        500 * SCALAR_7,
        1_000 * SCALAR_7,
        0,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    let before = f.market.get_position(&f.alice, &false);
    assert!(f
        .market
        .try_liquidate(&f.alice, &false, &f.keeper, &(START + 1))
        .is_err());
    assert_eq!(f.market.get_position(&f.alice, &false), before);
    f.assert_conservation();
}

#[test]
fn execution_needs_only_keeper_auth_and_direct_user_execution_is_blocked() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        100 * SCALAR_7,
        500 * SCALAR_7,
        0,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.env.mock_auths(&[]);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    assert!(f.exchange.try_cancel_order(&f.alice, &id).is_err());
    assert!(f
        .market
        .try_increase(
            &f.alice,
            &true,
            &(100 * SCALAR_7),
            &(500 * SCALAR_7),
            &PRICE_SCALE,
            &true,
            &(START + 1)
        )
        .is_err());
    f.env.mock_auths(&[MockAuth {
        address: &f.keeper,
        invoke: &MockAuthInvoke {
            contract: &f.exchange.address,
            fn_name: "execute_order",
            args: (f.keeper.clone(), id).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.exchange.execute_order(&f.keeper, &id);
    assert_eq!(
        f.market.get_position(&f.alice, &true).size_usd,
        500 * SCALAR_7
    );
    f.assert_conservation();
}

#[test]
fn pending_orders_are_serialized_and_amendments_use_new_timestamp() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        100 * SCALAR_7,
        500 * SCALAR_7,
        0,
    );
    assert!(f
        .exchange
        .try_create_order(
            &f.alice,
            &f.market.address,
            &MARKET_DECREASE,
            &true,
            &0,
            &SCALAR_7,
            &1,
            &0,
            &0,
            &true,
            &0,
            &1
        )
        .is_err());
    f.publish(START + 1, PRICE_SCALE);
    f.time(START + 2);
    f.exchange
        .update_order(&f.alice, &id, &(2 * PRICE_SCALE), &0, &0);
    assert_eq!(f.exchange.get_order(&id).created_at, START + 1);
    assert_eq!(f.exchange.get_order(&id).price_timestamp, START + 2);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.publish(START + 2, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    assert!(f
        .exchange
        .try_update_order(&f.alice, &id, &PRICE_SCALE, &0, &0)
        .is_err());
}

#[test]
fn twap_uses_scheduled_timestamps_and_never_reuses_escrow() {
    let f = Fixture::default();
    let id = f.exchange.create_order(
        &f.alice,
        &f.market.address,
        &TWAP_INCREASE,
        &true,
        &(500 * SCALAR_7),
        &(1_000 * SCALAR_7),
        &(3 * PRICE_SCALE),
        &0,
        &SCALAR_7,
        &true,
        &60,
        &2,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    assert_eq!(f.exchange.get_order(&id).price_timestamp, START + 61);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.time(START + 120);
    f.publish(START + 120, 2 * PRICE_SCALE);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.publish(START + 61, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    assert_eq!(
        f.market.get_position(&f.alice, &true).size_usd,
        1_000 * SCALAR_7
    );
    assert_eq!(f.balance(&f.quote, &f.keeper), SCALAR_7);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 0);
    f.assert_conservation();
}

#[test]
fn option_orders_validate_both_timestamps_and_only_settle_at_expiry() {
    let f = Fixture::default();
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        OptionKind::Put,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        3 * SCALAR_7,
    );
    f.time(START + 10);
    let sell = f.option(
        &f.bob,
        OrderSide::Sell,
        OptionKind::Put,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        2 * SCALAR_7,
    );
    f.publish(START + 1, PRICE_SCALE);
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    assert_eq!(f.options.get_order(&buy).status, OrderStatus::Open);
    assert_eq!(f.balance(&f.quote, &f.options.address), 23 * SCALAR_7);
    f.publish(START + 10, PRICE_SCALE);
    assert!(f.options.try_match_orders(&f.alice, &buy, &sell).is_err());
    let id = f.options.match_orders(&f.keeper, &buy, &sell);
    assert!(f.options.try_cancel_order(&f.alice, &buy).is_err());
    assert_eq!(
        f.options.try_settle(&id),
        Err(Ok(soroban_sdk::Error::from_contract_error(
            lumix_options::Error::NotExpired as u32
        )))
    );
    f.time(EXPIRY - 1);
    assert!(f.options.try_settle(&id).is_err());
    f.time(EXPIRY + 1_000);
    f.publish(EXPIRY + 1_000, 3 * PRICE_SCALE);
    assert!(f.options.try_settle(&id).is_err());
    f.publish(EXPIRY, PRICE_SCALE);
    f.options.set_paused(&true);
    f.options.set_pair(&f.base, &f.quote, &false);
    f.env.mock_auths(&[]);
    let p = f.options.settle(&id);
    assert_eq!(p.status, PositionStatus::Settled);
    assert_eq!(p.settlement_price, PRICE_SCALE);
    assert_eq!(p.buyer_payout, 10 * SCALAR_7);
    assert_eq!(p.seller_payout, 10 * SCALAR_7);
    assert_eq!(f.balance(&f.quote, &f.options.address), 0);
    assert!(f.options.try_settle(&id).is_err());
    f.assert_conservation();
}

#[soroban_sdk::contract]
struct Source;
#[soroban_sdk::contractimpl]
impl Source {
    pub fn price_at(
        _env: soroban_sdk::Env,
        _asset: Address,
        timestamp: u64,
    ) -> lumix_interfaces::OraclePrice {
        lumix_interfaces::OraclePrice {
            min: PRICE_SCALE,
            max: PRICE_SCALE,
            timestamp,
        }
    }
}

#[test]
fn external_source_allows_validation_without_publishing_prices() {
    let f = Fixture::default();
    let source = f.env.register(Source, ());
    f.oracle.set_source(&Some(source));
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        2_000 * SCALAR_7,
        0,
    );
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = f.option(
        &f.bob,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    f.time(START + 100);
    f.exchange.execute_order(&f.keeper, &id);
    let option_id = f.options.match_orders(&f.keeper, &buy, &sell);
    f.time(START + 200);
    let close = f.order(&f.alice, MARKET_DECREASE, true, 0, 2_000 * SCALAR_7, 0);
    f.exchange.execute_order(&f.keeper, &close);
    f.time(EXPIRY);
    f.options.settle(&option_id);
    assert!(f.market.try_get_position(&f.alice, &true).is_err());
    assert_eq!(
        f.oracle.quote_at(&f.base, &(START + 1)).timestamp,
        START + 1
    );
    f.assert_conservation();
}

#[test]
fn lp_withdrawals_do_not_spend_trader_collateral() {
    let f = Fixture::default();
    let pool_before = f.pool.pool_value();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        50_000 * SCALAR_7,
        1_000 * SCALAR_7,
        0,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    let collateral = f.market.get_position(&f.alice, &true).collateral_amount;
    assert_eq!(f.pool.collateral(), collateral);
    // A $50k trader deposit is not a $50k windfall for LPs.
    assert!(f.pool.pool_value() < pool_before + 100 * SCALAR_7);
    let shares = f.pool.balance(&f.lp) / 2;
    let (_, quote_out) = f.pool.withdraw(&f.lp, &shares, &0, &0);
    assert!(quote_out <= 50_100 * SCALAR_7);
    assert!(f.balance(&f.quote, &f.pool.address) >= collateral);
    f.assert_conservation();
}

#[test]
fn invalid_orders_and_mismatched_oracle_reject_before_escrow() {
    let f = Fixture::default();
    let before = f.balance(&f.quote, &f.alice);
    for (kind, collat, size, fee, isolated) in [
        (99, 1, 1, 0, true),
        (0, -1, 1, 0, true),
        (0, 1, -1, 0, true),
        (0, 1, 1, -1, true),
        (0, 1, 1, 0, false),
    ] {
        assert!(f
            .exchange
            .try_create_order(
                &f.alice,
                &f.market.address,
                &kind,
                &true,
                &collat,
                &size,
                &PRICE_SCALE,
                &0,
                &fee,
                &isolated,
                &0,
                &1
            )
            .is_err());
    }
    let ex = f.env.register(
        lumix_exchange::Exchange,
        (f.admin.clone(), Address::generate(&f.env), f.quote.clone()),
    );
    let bad = lumix_exchange::ExchangeClient::new(&f.env, &ex);
    assert!(bad
        .try_create_order(
            &f.alice,
            &f.market.address,
            &0,
            &true,
            &SCALAR_7,
            &SCALAR_7,
            &PRICE_SCALE,
            &0,
            &0,
            &true,
            &0,
            &1
        )
        .is_err());
    assert_eq!(f.balance(&f.quote, &f.alice), before);
}

fn measured(f: &Fixture, label: &str, run: impl FnOnce()) {
    let mut budget = f.env.cost_estimate().budget();
    // Debug/auth snapshots consume the host's shadow budget. Keep that test
    // overhead unlimited; invocation limits below meter actual contract work.
    // SDK mainnet invocation resource enforcement remains enabled.
    budget.reset_unlimited();
    run();
    let cpu = budget.cpu_instruction_cost();
    let memory = budget.memory_bytes_cost();
    println!("{label}: {cpu} CPU instructions, {memory} memory bytes");
    assert!(cpu <= 100_000_000);
    assert!(memory <= 40 * 1024 * 1024);

    budget.reset_unlimited();
}

#[test]
fn individual_transactions_fit_resource_limits() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        100 * SCALAR_7,
        500 * SCALAR_7,
        SCALAR_7,
    );
    measured(&f, "publish price", || f.publish(START + 1, PRICE_SCALE));
    measured(&f, "execute perp open", || {
        f.exchange.execute_order(&f.keeper, &id)
    });
    f.time(START + 2);
    f.publish(START + 2, PRICE_SCALE);
    let close = f.order(&f.alice, MARKET_DECREASE, true, 0, 500 * SCALAR_7, 0);
    measured(&f, "execute perp close", || {
        f.exchange.execute_order(&f.keeper, &close)
    });
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        false,
        100 * SCALAR_7,
        1_000 * SCALAR_7,
        0,
    );
    f.exchange.execute_order(&f.keeper, &id);
    f.time(START + 3);
    f.publish(START + 3, 2 * PRICE_SCALE);
    measured(&f, "liquidate perp", || {
        f.market
            .liquidate(&f.alice, &false, &f.keeper, &(START + 3));
    });
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = f.option(
        &f.bob,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    // Return the ID through a cell to keep the measurement helper small.
    let result = std::cell::Cell::new(0);
    measured(&f, "validate option", || {
        result.set(f.options.match_orders(&f.keeper, &buy, &sell))
    });
    let option_id = result.get();
    f.time(EXPIRY);
    f.publish(EXPIRY, 2 * PRICE_SCALE);
    measured(&f, "settle option", || {
        f.options.settle(&option_id);
    });
}

#[test]
fn unpegged_collateral_requires_same_timestamp_and_options_use_base_quote_ratio() {
    let f = Fixture::new(false);
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        1_000 * SCALAR_7,
        0,
    );
    f.publish(START + 1, PRICE_SCALE);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 500 * SCALAR_7);
    f.oracle.set_price_at(
        &f.keeper,
        &f.quote,
        &(START + 1),
        &(2 * PRICE_SCALE),
        &(2 * PRICE_SCALE),
    );
    f.exchange.execute_order(&f.keeper, &id);
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        OptionKind::Put,
        10 * SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = f.option(
        &f.bob,
        OrderSide::Sell,
        OptionKind::Put,
        10 * SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let option_id = f.options.match_orders(&f.keeper, &buy, &sell);
    assert_eq!(
        f.options.get_position(&option_id).buyer_price,
        PRICE_SCALE / 2
    );
    f.time(EXPIRY + 1_000);
    f.publish(EXPIRY, PRICE_SCALE);
    assert!(f.options.try_settle(&option_id).is_err());
    f.oracle.set_price_at(
        &f.keeper,
        &f.quote,
        &EXPIRY,
        &(4 * PRICE_SCALE),
        &(4 * PRICE_SCALE),
    );
    let p = f.options.settle(&option_id);
    assert_eq!(p.settlement_price, PRICE_SCALE / 4);
    assert_eq!(p.buyer_payout, 75 * SCALAR_7 / 10);
    f.assert_conservation();
}

#[test]
fn option_validation_needs_keeper_signature_without_trader_resigning() {
    let f = Fixture::default();
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = f.option(
        &f.bob,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.env.mock_auths(&[]);
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    f.env.mock_auths(&[MockAuth {
        address: &f.keeper,
        invoke: &MockAuthInvoke {
            contract: &f.options.address,
            fn_name: "match_orders",
            args: (f.keeper.clone(), buy, sell).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    let id = f.options.match_orders(&f.keeper, &buy, &sell);
    assert_eq!(f.options.get_position(&id).status, PositionStatus::Open);
}

#[test]
fn missing_price_does_not_allow_trader_cancellation_or_update_by_someone_else() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        100 * SCALAR_7,
        500 * SCALAR_7,
        SCALAR_7,
    );
    assert!(f.exchange.try_cancel_order(&f.bob, &id).is_err());
    assert!(f
        .exchange
        .try_update_order(&f.bob, &id, &PRICE_SCALE, &0, &SCALAR_7)
        .is_err());
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 101 * SCALAR_7);
    assert!(f.exchange.try_execute_order(&f.bob, &id).is_err());
    f.assert_conservation();
}

#[soroban_sdk::contract]
struct FailingSource;
#[soroban_sdk::contractimpl]
impl FailingSource {
    pub fn __constructor(env: soroban_sdk::Env, fail_asset: Address) {
        env.storage().instance().set(&0u32, &fail_asset);
    }
    pub fn price_at(
        env: soroban_sdk::Env,
        asset: Address,
        timestamp: u64,
    ) -> lumix_interfaces::OraclePrice {
        let blocked: Address = env.storage().instance().get(&0u32).unwrap();
        lumix_interfaces::OraclePrice {
            min: if asset == blocked { 0 } else { PRICE_SCALE },
            max: PRICE_SCALE,
            timestamp,
        }
    }
}

#[test]
fn source_failure_rolls_back_escrow_transfers_and_partial_oracle_cache() {
    let f = Fixture::new(false);
    let source = f.env.register(FailingSource, (f.quote.clone(),));
    f.oracle.set_source(&Some(source));
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        1_000 * SCALAR_7,
        SCALAR_7,
    );
    let pool_before = f.balance(&f.quote, &f.pool.address);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 501 * SCALAR_7);
    assert_eq!(f.balance(&f.quote, &f.pool.address), pool_before);
    assert_eq!(f.pool.collateral(), 0);
    assert_eq!(f.balance(&f.quote, &f.keeper), 0);
    f.oracle.set_source(&None);
    assert!(f.oracle.try_quote_at(&f.base, &(START + 1)).is_err());
    f.exchange.cancel_order(&f.alice, &id);
    f.assert_conservation();
}

#[test]
fn source_resolution_with_two_dynamic_assets_fits_resource_limits() {
    let f = Fixture::new(false);
    let source = f.env.register(Source, ());
    f.oracle.set_source(&Some(source));
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        1_000 * SCALAR_7,
        SCALAR_7,
    );
    measured(&f, "perp open resolving both assets", || {
        f.exchange.execute_order(&f.keeper, &id)
    });
    f.assert_conservation();
}

#[soroban_sdk::contract]
struct UnregisteredMarket;
#[soroban_sdk::contractimpl]
impl UnregisteredMarket {
    pub fn config(env: soroban_sdk::Env) -> lumix_interfaces::MarketConfigView {
        env.storage().instance().get(&0u32).unwrap()
    }
}

#[test]
fn unregistered_market_cannot_receive_escrow_or_impersonate_a_registered_market() {
    let f = Fixture::default();
    let fake = f.env.register(UnregisteredMarket, ());
    let metadata = f.market.config();
    // It advertises all the same pool/oracle/token addresses as the real market.
    f.env
        .as_contract(&fake, || f.env.storage().instance().set(&0u32, &metadata));
    let before = f.balance(&f.quote, &f.alice);
    assert!(f
        .exchange
        .try_create_order(
            &f.alice,
            &fake,
            &MARKET_INCREASE,
            &true,
            &(500 * SCALAR_7),
            &(1_000 * SCALAR_7),
            &(2 * PRICE_SCALE),
            &0,
            &SCALAR_7,
            &true,
            &0,
            &1
        )
        .is_err());
    assert_eq!(f.balance(&f.quote, &f.alice), before);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 0);
    assert!(f.exchange.try_set_factory(&fake).is_err());
}

#[test]
fn cancellation_uses_pinned_escrow_token_even_if_market_metadata_changes() {
    let f = Fixture::default();
    let before = f.balance(&f.quote, &f.alice);
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        1_000 * SCALAR_7,
        SCALAR_7,
    );
    let mut changed = f.market.config();
    changed.short_token = f.base.clone();
    f.env.as_contract(&f.market.address, || {
        f.env
            .storage()
            .instance()
            .set(&lumix_market::DataKey::Config, &changed);
    });
    f.publish(START + 1, PRICE_SCALE);
    assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
    f.exchange.cancel_order(&f.alice, &id);
    assert_eq!(f.balance(&f.quote, &f.alice), before);
    assert_eq!(f.balance(&f.quote, &f.exchange.address), 0);
    f.assert_conservation();
}

#[test]
fn disabled_factory_blocks_new_risk_but_allows_existing_position_closes() {
    let f = Fixture::default();
    let id = f.order(
        &f.alice,
        MARKET_INCREASE,
        true,
        500 * SCALAR_7,
        1_000 * SCALAR_7,
        0,
    );
    f.publish(START + 1, PRICE_SCALE);
    f.exchange.execute_order(&f.keeper, &id);
    f.factory.set_enabled(&f.base, &false);
    assert!(!f.factory.get_market(&f.base).enabled);
    assert!(f
        .exchange
        .try_create_order(
            &f.bob,
            &f.market.address,
            &MARKET_INCREASE,
            &true,
            &(500 * SCALAR_7),
            &(1_000 * SCALAR_7),
            &(2 * PRICE_SCALE),
            &0,
            &0,
            &true,
            &0,
            &1
        )
        .is_err());
    let close = f.order(&f.alice, MARKET_DECREASE, true, 0, 1_000 * SCALAR_7, 0);
    f.exchange.execute_order(&f.keeper, &close);
    assert!(f.market.try_get_position(&f.alice, &true).is_err());
    f.assert_conservation();
}

#[test]
fn factory_binding_requires_admin_signature() {
    let f = Fixture::default();
    f.env.mock_auths(&[]);
    assert!(f.exchange.try_set_factory(&f.factory.address).is_err());
}
