use crate::harness::*;
use lumix_core::{PRICE_SCALE, SCALAR_7};
use lumix_exchange::{MARKET_DECREASE, MARKET_INCREASE};
use lumix_options::{OptionKind, OrderSide, PositionStatus};
use lumix_oracle::{Oracle, OracleClient};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    Address, Env,
};

/// Reference-map differential test of arbitrary out-of-order oracle publications.
pub fn oracle_history(data: &[u8]) {
    let mut env = Env::default();
    env.set_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_timestamp(10_000);
    let admin = Address::generate(&env);
    let assets = [Address::generate(&env), Address::generate(&env)];
    let id = env.register(Oracle, (admin.clone(), 600u64, u32::MAX, 2u32));
    let oracle = OracleClient::new(&env, &id);
    let mut model = std::collections::BTreeMap::new();
    let mut latest = [None, None];
    for chunk in data.chunks_exact(4).take(64) {
        env.cost_estimate().budget().reset_default();
        let asset = usize::from(chunk[0] % 2);
        let ts = u64::from(chunk[1]);
        let price = (i128::from(chunk[2]) + 1) * PRICE_SCALE / 10;
        let max = if chunk[3] % 5 == 0 {
            price - 1
        } else {
            price + i128::from(chunk[3])
        };
        let already = model.contains_key(&(asset, ts));
        let result = oracle.try_set_price_at(&admin, &assets[asset], &ts, &price, &max);
        if already || max >= price {
            assert!(result.is_ok());
            model.entry((asset, ts)).or_insert((price, max));
            if latest[asset].map(|previous| ts > previous).unwrap_or(true) {
                latest[asset] = Some(ts);
            }
        } else {
            assert!(result.is_err());
        }
        for (&(a, t), &(min, max)) in &model {
            let actual = oracle.quote_at(&assets[a], &t);
            assert_eq!((actual.min, actual.max, actual.timestamp), (min, max, t));
        }
        if let Some(t) = latest[asset] {
            assert_eq!(oracle.quote_unchecked(&assets[asset]).timestamp, t);
        }
    }
}

/// Bounded sequence of pending requests, cancellations, missing observations,
/// execution, and liquidation. Every step checks escrow and token conservation.
pub fn perp_lifecycle(data: &[u8]) {
    let f = Fixture::default();
    let mut now = START + 1;
    for chunk in data.chunks_exact(4).take(8) {
        let long = chunk[0] % 2 == 0;
        let collateral = (100 + i128::from(chunk[1])) * SCALAR_7;
        let size = collateral * (2 + i128::from(chunk[2] % 4));
        let fee = i128::from(chunk[3] % 3) * SCALAR_7;
        f.time(now);
        let before = f.balance(&f.quote, &f.alice);
        let id = f.order(&f.alice, MARKET_INCREASE, long, collateral, size, fee);
        assert!(f.market.try_get_position(&f.alice, &long).is_err());
        assert_eq!(f.balance(&f.quote, &f.exchange.address), collateral + fee);
        assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
        if chunk[0] % 3 == 0 {
            f.exchange.cancel_order(&f.alice, &id);
            assert_eq!(f.balance(&f.quote, &f.alice), before);
        } else {
            f.time(now + u64::from(chunk[3]) + 1);
            f.publish(now, PRICE_SCALE);
            f.publish(now, 2 * PRICE_SCALE);
            f.exchange.execute_order(&f.keeper, &id);
            assert!(f.exchange.try_execute_order(&f.keeper, &id).is_err());
            assert_eq!(
                f.pool.collateral(),
                f.market.get_position(&f.alice, &long).collateral_amount
            );
            now += 300;
            f.time(now);
            let close = f.order(&f.alice, MARKET_DECREASE, long, 0, size, fee);
            assert!(f.exchange.try_execute_order(&f.keeper, &close).is_err());
            if chunk[0] % 5 == 0 {
                // Liquidate while the close is pending, using a distinct time.
                f.time(now + 1);
                let price = if long {
                    PRICE_SCALE / 10
                } else {
                    PRICE_SCALE * 2
                };
                f.publish(now + 1, price);
                f.market.liquidate(&f.alice, &long, &f.keeper, &(now + 1));
                f.publish(now, PRICE_SCALE);
                assert!(f.exchange.try_execute_order(&f.keeper, &close).is_err());
                f.exchange.cancel_order(&f.alice, &close);
            } else {
                f.publish(now, PRICE_SCALE);
                f.exchange.execute_order(&f.keeper, &close);
            }
            assert!(f.market.try_get_position(&f.alice, &long).is_err());
        }
        assert_eq!(f.pool.collateral(), 0);
        assert_eq!(f.balance(&f.quote, &f.exchange.address), 0);
        assert_eq!(f.market.get_oi().long_usd + f.market.get_oi().short_usd, 0);
        f.assert_conservation();
        now += 1_000;
    }
}

/// Independently compute European payouts and check every escrow token is paid
/// exactly once across strike/spot, call/put, timing and cancellation variants.
pub fn option_lifecycle(data: &[u8]) {
    if data.len() < 6 {
        return;
    }
    let f = Fixture::default();
    let call = data[0].is_multiple_of(2);
    let kind = if call {
        OptionKind::Call
    } else {
        OptionKind::Put
    };
    let amount = (i128::from(data[1]) + 1) * SCALAR_7;
    let strike = (i128::from(data[2]) + 1) * PRICE_SCALE / 10;
    let spot = (i128::from(data[3]) + 1) * PRICE_SCALE / 10;
    let premium = (i128::from(data[4]) + 1) * SCALAR_7;
    let buy = f.option(
        &f.alice,
        OrderSide::Buy,
        kind.clone(),
        amount,
        strike,
        premium,
    );
    f.time(START + 2);
    let sell = f.option(&f.bob, OrderSide::Sell, kind, amount, strike, premium);
    if data[5].is_multiple_of(5) {
        f.options.cancel_order(&f.alice, &buy);
        f.options.cancel_order(&f.bob, &sell);
    } else {
        assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
        f.publish(START + 1, PRICE_SCALE);
        assert!(f.options.try_match_orders(&f.keeper, &buy, &sell).is_err());
        f.publish(START + 2, PRICE_SCALE);
        let id = f.options.match_orders(&f.keeper, &buy, &sell);
        assert!(f.options.try_settle(&id).is_err());
        f.time(EXPIRY + u64::from(data[5]));
        assert!(f.options.try_settle(&id).is_err());
        f.publish(EXPIRY, spot);
        f.publish(EXPIRY, PRICE_SCALE * 100);
        let position = f.options.settle(&id);
        let expected = if call {
            if spot > strike {
                amount * (spot - strike) / spot
            } else {
                0
            }
        } else if strike > spot {
            amount * (strike - spot) / PRICE_SCALE
        } else {
            0
        };
        assert_eq!(position.buyer_payout, expected);
        assert_eq!(
            position.buyer_payout + position.seller_payout,
            position.collateral
        );
        assert_eq!(position.settlement_price, spot);
        assert_eq!(position.status, PositionStatus::Settled);
        assert!(f.options.try_settle(&id).is_err());
    }
    assert_eq!(f.balance(&f.base, &f.options.address), 0);
    assert_eq!(f.balance(&f.quote, &f.options.address), 0);
    f.assert_conservation();
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn immutable_oracle_matches_reference_model(data in proptest::collection::vec(any::<u8>(), 4..128)) {
            oracle_history(&data);
        }
        #[test]
        fn perp_request_sequences_preserve_funds(data in proptest::collection::vec(any::<u8>(), 4..32)) {
            perp_lifecycle(&data);
        }
        #[test]
        fn european_options_match_independent_payoff_model(data in proptest::collection::vec(any::<u8>(), 6..12)) {
            option_lifecycle(&data);
        }
    }
}
