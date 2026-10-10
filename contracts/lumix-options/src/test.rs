#![cfg(test)]

use super::*;
use lumix_core::{PRICE_SCALE, SCALAR_7};
use lumix_oracle::{Oracle, OracleClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};

const START: u64 = 1_000_000;
const EXPIRY: u64 = START + 86_400;
const INITIAL: i128 = 1_000 * SCALAR_7;

struct Fixture {
    env: Env,
    admin: Address,
    keeper: Address,
    buyer: Address,
    seller: Address,
    stranger: Address,
    base: Address,
    quote: Address,
    oracle: OracleClient<'static>,
    options_id: Address,
    options: OptionsClient<'static>,
    base_token: TokenClient<'static>,
    quote_token: TokenClient<'static>,
}

fn mint(env: &Env, token: &Address, to: &Address, amount: i128) {
    StellarAssetClient::new(env, token).mint(to, &amount);
}

fn setup() -> Fixture {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    env.mock_all_auths();
    env.ledger().set_timestamp(START);
    let admin = Address::generate(&env);
    let keeper = Address::generate(&env);
    let buyer = Address::generate(&env);
    let seller = Address::generate(&env);
    let stranger = Address::generate(&env);
    let issuer = Address::generate(&env);
    let base = env
        .register_stellar_asset_contract_v2(issuer.clone())
        .address();
    let quote = env.register_stellar_asset_contract_v2(issuer).address();
    for user in [&buyer, &seller] {
        mint(&env, &base, user, INITIAL);
        mint(&env, &quote, user, INITIAL);
    }

    let oracle_id = env.register(Oracle, (admin.clone(), 600u64, 20_000u32, 0u32));
    let oracle = OracleClient::new(&env, &oracle_id);
    oracle.set_keeper(&keeper, &true);
    oracle.set_fixed_price(&quote, &PRICE_SCALE);
    oracle.set_price_at(&admin, &base, &START, &PRICE_SCALE, &PRICE_SCALE);
    let options_id = env.register(Options, (admin.clone(), oracle_id));
    let options = OptionsClient::new(&env, &options_id);
    options.set_pair(&base, &quote, &true);
    options.set_expiry(&base, &quote, &EXPIRY, &true);
    options.set_keeper(&keeper, &true);

    let base_token = TokenClient::new(&env, &base);
    let quote_token = TokenClient::new(&env, &quote);
    Fixture {
        env,
        admin,
        keeper,
        buyer,
        seller,
        stranger,
        base,
        quote,
        oracle,
        options_id,
        options,
        base_token,
        quote_token,
    }
}

fn request(
    f: &Fixture,
    side: OrderSide,
    kind: OptionKind,
    amount: i128,
    strike: i128,
    premium: i128,
) -> OrderRequest {
    OrderRequest {
        amount,
        base: f.base.clone(),
        expiry: EXPIRY,
        kind,
        premium,
        quote: f.quote.clone(),
        strike,
        side,
    }
}

fn create(
    f: &Fixture,
    trader: &Address,
    side: OrderSide,
    kind: OptionKind,
    amount: i128,
    strike: i128,
    premium: i128,
) -> u32 {
    f.options
        .create_order(trader, &request(f, side, kind, amount, strike, premium))
}

fn matched(
    f: &Fixture,
    kind: OptionKind,
    amount: i128,
    strike: i128,
    bid: i128,
    ask: i128,
    seller_first: bool,
) -> u32 {
    let (buy, sell) = if seller_first {
        let sell = create(
            f,
            &f.seller,
            OrderSide::Sell,
            kind.clone(),
            amount,
            strike,
            ask,
        );
        let buy = create(f, &f.buyer, OrderSide::Buy, kind, amount, strike, bid);
        (buy, sell)
    } else {
        let buy = create(
            f,
            &f.buyer,
            OrderSide::Buy,
            kind.clone(),
            amount,
            strike,
            bid,
        );
        let sell = create(f, &f.seller, OrderSide::Sell, kind, amount, strike, ask);
        (buy, sell)
    };
    f.options.match_orders(&f.keeper, &buy, &sell)
}

#[test]
fn supported_pair_and_expiry_are_admin_controlled() {
    let f = setup();
    assert!(f.options.is_pair_supported(&f.base, &f.quote));
    assert!(f.options.is_expiry_supported(&f.base, &f.quote, &EXPIRY));

    let other = Address::generate(&f.env);
    let req = OrderRequest {
        quote: other,
        ..request(
            &f,
            OrderSide::Buy,
            OptionKind::Call,
            SCALAR_7,
            PRICE_SCALE,
            SCALAR_7,
        )
    };
    assert!(f.options.try_create_order(&f.buyer, &req).is_err());
    assert!(f
        .options
        .try_set_expiry(&f.base, &f.quote, &(START + MAX_EXPIRY_SECONDS + 1), &true,)
        .is_err());
    assert!(f.options.try_set_pair(&f.base, &f.base, &true).is_err());

    f.options.set_expiry(&f.base, &f.quote, &EXPIRY, &false);
    assert!(f
        .options
        .try_create_order(
            &f.buyer,
            &request(
                &f,
                OrderSide::Buy,
                OptionKind::Call,
                SCALAR_7,
                PRICE_SCALE,
                SCALAR_7,
            ),
        )
        .is_err());
}

#[test]
fn admin_functions_require_admin_signature() {
    let env = Env::default();
    env.cost_estimate().budget().reset_unlimited();
    let admin = Address::generate(&env);
    let base = Address::generate(&env);
    let quote = Address::generate(&env);
    let id = env.register(Options, (admin, Address::generate(&env)));
    let client = OptionsClient::new(&env, &id);
    assert!(client.try_set_pair(&base, &quote, &true).is_err());
}

#[test]
fn buy_order_escrows_premium_and_cancel_refunds() {
    let f = setup();
    let premium = 3 * SCALAR_7;
    let before = f.quote_token.balance(&f.buyer);
    let id = create(
        &f,
        &f.buyer,
        OrderSide::Buy,
        OptionKind::Call,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        premium,
    );
    assert_eq!(f.quote_token.balance(&f.buyer), before - premium);
    assert_eq!(f.quote_token.balance(&f.options_id), premium);
    f.options.cancel_order(&f.buyer, &id);
    assert_eq!(f.quote_token.balance(&f.buyer), before);
    assert_eq!(f.options.get_order(&id).status, OrderStatus::Cancelled);
    assert!(f.options.try_cancel_order(&f.buyer, &id).is_err());
}

#[test]
fn call_writer_escrows_base_and_cancel_refunds() {
    let f = setup();
    let amount = 10 * SCALAR_7;
    let before = f.base_token.balance(&f.seller);
    let id = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Call,
        amount,
        2 * PRICE_SCALE,
        SCALAR_7,
    );
    assert_eq!(f.base_token.balance(&f.seller), before - amount);
    assert_eq!(f.options.get_order(&id).escrowed, amount);
    f.options.cancel_order(&f.seller, &id);
    assert_eq!(f.base_token.balance(&f.seller), before);
}

#[test]
fn put_writer_escrows_full_strike_notional() {
    let f = setup();
    let amount = 10 * SCALAR_7;
    let collateral = 40 * SCALAR_7;
    let before = f.quote_token.balance(&f.seller);
    let id = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Put,
        amount,
        4 * PRICE_SCALE,
        SCALAR_7,
    );
    assert_eq!(f.options.get_order(&id).escrowed, collateral);
    assert_eq!(f.quote_token.balance(&f.seller), before - collateral);
    f.options.cancel_order(&f.seller, &id);
    assert_eq!(f.quote_token.balance(&f.seller), before);
}

#[test]
fn invalid_order_values_pause_and_wrong_owner_are_rejected() {
    let f = setup();
    let mut req = request(
        &f,
        OrderSide::Buy,
        OptionKind::Call,
        0,
        PRICE_SCALE,
        SCALAR_7,
    );
    assert!(f.options.try_create_order(&f.buyer, &req).is_err());
    req.amount = SCALAR_7;
    req.premium = 0;
    assert!(f.options.try_create_order(&f.buyer, &req).is_err());

    let id = create(
        &f,
        &f.buyer,
        OrderSide::Buy,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    assert!(f.options.try_cancel_order(&f.stranger, &id).is_err());
    f.options.set_paused(&true);
    assert!(f
        .options
        .try_create_order(
            &f.seller,
            &request(
                &f,
                OrderSide::Sell,
                OptionKind::Call,
                SCALAR_7,
                PRICE_SCALE,
                SCALAR_7,
            ),
        )
        .is_err());
    // Risk reduction remains available while paused.
    f.options.cancel_order(&f.buyer, &id);
}

#[test]
fn seller_maker_price_refunds_buyers_bid_improvement() {
    let f = setup();
    let position_id = matched(
        &f,
        OptionKind::Call,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        3 * SCALAR_7,
        2 * SCALAR_7,
        true,
    );
    let position = f.options.get_position(&position_id);
    assert_eq!(position.premium, 2 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.buyer), INITIAL - 2 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.options_id), 2 * SCALAR_7);
    assert_eq!(f.base_token.balance(&f.options_id), 10 * SCALAR_7);
}

#[test]
fn buyer_maker_price_executes_at_bid() {
    let f = setup();
    let position_id = matched(
        &f,
        OptionKind::Call,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        3 * SCALAR_7,
        2 * SCALAR_7,
        false,
    );
    assert_eq!(f.options.get_position(&position_id).premium, 3 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.buyer), INITIAL - 3 * SCALAR_7);
}

#[test]
fn matcher_rejects_non_crossing_mismatched_self_and_reused_orders() {
    let f = setup();
    let buy = create(
        &f,
        &f.buyer,
        OrderSide::Buy,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        2 * SCALAR_7,
    );
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());

    let wrong_kind = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Put,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    assert!(f
        .options
        .try_match_orders(&f.keeper, &buy, &wrong_kind)
        .is_err());

    let wrong_amount = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Call,
        2 * SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    assert!(f
        .options
        .try_match_orders(&f.keeper, &buy, &wrong_amount)
        .is_err());

    let self_sell = create(
        &f,
        &f.buyer,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    assert!(f
        .options
        .try_match_orders(&f.keeper, &buy, &self_sell)
        .is_err());

    let good_sell = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    f.options.match_orders(&f.keeper, &buy, &good_sell);
    assert!(f
        .options
        .try_match_orders(&f.keeper, &buy, &good_sell)
        .is_err());
}

#[test]
fn matching_requires_an_active_unexpired_series_but_cancel_stays_available() {
    let f = setup();
    let buy = create(
        &f,
        &f.buyer,
        OrderSide::Buy,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );
    let sell = create(
        &f,
        &f.seller,
        OrderSide::Sell,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
    );

    f.options.set_paused(&true);
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    f.options.set_paused(&false);

    f.options.set_pair(&f.base, &f.quote, &false);
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    f.options.set_pair(&f.base, &f.quote, &true);

    f.options.set_expiry(&f.base, &f.quote, &EXPIRY, &false);
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    f.options.set_expiry(&f.base, &f.quote, &EXPIRY, &true);

    f.env.ledger().set_timestamp(EXPIRY);
    assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
    f.options.cancel_order(&f.buyer, &buy);
    f.options.cancel_order(&f.seller, &sell);
}

#[test]
fn itm_call_settles_in_base_and_releases_premium() {
    let f = setup();
    let position_id = matched(
        &f,
        OptionKind::Call,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        3 * SCALAR_7,
        2 * SCALAR_7,
        true,
    );
    f.env.ledger().set_timestamp(EXPIRY);
    f.oracle.set_price_at(
        &f.keeper,
        &f.base,
        &EXPIRY,
        &(4 * PRICE_SCALE),
        &(4 * PRICE_SCALE),
    );
    let settled = f.options.settle(&position_id);
    assert_eq!(settled.buyer_payout, 5 * SCALAR_7);
    assert_eq!(settled.seller_payout, 5 * SCALAR_7);
    assert_eq!(settled.status, PositionStatus::Settled);
    assert_eq!(f.base_token.balance(&f.buyer), INITIAL + 5 * SCALAR_7);
    assert_eq!(f.base_token.balance(&f.seller), INITIAL - 5 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.seller), INITIAL + 2 * SCALAR_7);
    assert_eq!(f.base_token.balance(&f.options_id), 0);
    assert_eq!(f.quote_token.balance(&f.options_id), 0);
    assert!(f.options.try_settle(&position_id).is_err());
}

#[test]
fn itm_put_settles_in_quote_and_remains_fully_backed() {
    let f = setup();
    let position_id = matched(
        &f,
        OptionKind::Put,
        10 * SCALAR_7,
        4 * PRICE_SCALE,
        2 * SCALAR_7,
        2 * SCALAR_7,
        true,
    );
    f.env.ledger().set_timestamp(EXPIRY);
    f.oracle
        .set_price_at(&f.keeper, &f.base, &EXPIRY, &PRICE_SCALE, &PRICE_SCALE);
    let settled = f.options.settle(&position_id);
    assert_eq!(settled.collateral, 40 * SCALAR_7);
    assert_eq!(settled.buyer_payout, 30 * SCALAR_7);
    assert_eq!(settled.seller_payout, 10 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.buyer), INITIAL + 28 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.seller), INITIAL - 28 * SCALAR_7);
    assert_eq!(f.quote_token.balance(&f.options_id), 0);
}

#[test]
fn out_of_money_call_and_put_return_all_collateral_to_writer() {
    let call = setup();
    let call_id = matched(
        &call,
        OptionKind::Call,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        SCALAR_7,
        SCALAR_7,
        true,
    );
    call.env.ledger().set_timestamp(EXPIRY);
    call.oracle.set_price_at(
        &call.keeper,
        &call.base,
        &EXPIRY,
        &PRICE_SCALE,
        &PRICE_SCALE,
    );
    let call_position = call.options.settle(&call_id);
    assert_eq!(call_position.buyer_payout, 0);
    assert_eq!(call_position.seller_payout, 10 * SCALAR_7);

    let put = setup();
    let put_id = matched(
        &put,
        OptionKind::Put,
        10 * SCALAR_7,
        2 * PRICE_SCALE,
        SCALAR_7,
        SCALAR_7,
        true,
    );
    put.env.ledger().set_timestamp(EXPIRY);
    put.oracle.set_price_at(
        &put.keeper,
        &put.base,
        &EXPIRY,
        &(3 * PRICE_SCALE),
        &(3 * PRICE_SCALE),
    );
    let put_position = put.options.settle(&put_id);
    assert_eq!(put_position.buyer_payout, 0);
    assert_eq!(put_position.seller_payout, 20 * SCALAR_7);
}

#[test]
fn settlement_timing_uniqueness_and_missing_price_are_enforced() {
    let f = setup();
    let position_id = matched(
        &f,
        OptionKind::Call,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
        SCALAR_7,
        true,
    );
    assert!(f
        .oracle
        .try_set_price_at(&f.keeper, &f.base, &EXPIRY, &PRICE_SCALE, &PRICE_SCALE)
        .is_err());
    assert!(f.options.try_settle(&position_id).is_err());
    f.env.ledger().set_timestamp(EXPIRY);
    assert!(f.options.try_settle(&position_id).is_err());
    assert!(f
        .oracle
        .try_set_price_at(&f.stranger, &f.base, &EXPIRY, &PRICE_SCALE, &PRICE_SCALE)
        .is_err());
    f.oracle
        .set_price_at(&f.keeper, &f.base, &EXPIRY, &PRICE_SCALE, &PRICE_SCALE);
    f.oracle.set_price_at(
        &f.admin,
        &f.base,
        &EXPIRY,
        &(PRICE_SCALE * 2),
        &(PRICE_SCALE * 2),
    );
    assert_eq!(
        f.options.get_settlement(&f.base, &f.quote, &EXPIRY),
        PRICE_SCALE
    );
    assert_eq!(f.options.settle(&position_id).settlement_price, PRICE_SCALE);
}

#[test]
fn disabled_series_can_still_lock_price_and_settle_existing_positions() {
    let f = setup();
    let position_id = matched(
        &f,
        OptionKind::Put,
        SCALAR_7,
        PRICE_SCALE,
        SCALAR_7,
        SCALAR_7,
        true,
    );
    f.options.set_expiry(&f.base, &f.quote, &EXPIRY, &false);
    f.options.set_pair(&f.base, &f.quote, &false);
    f.env.ledger().set_timestamp(EXPIRY);
    f.oracle.set_price_at(
        &f.keeper,
        &f.base,
        &EXPIRY,
        &(PRICE_SCALE / 2),
        &(PRICE_SCALE / 2),
    );
    assert_eq!(
        f.options.settle(&position_id).status,
        PositionStatus::Settled
    );
}
