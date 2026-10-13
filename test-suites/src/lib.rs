pub mod harness;
pub mod scenarios;

#[cfg(test)]
mod flow_tests;

#[cfg(test)]
mod legacy {

    use lumix_core::{PRICE_SCALE, SCALAR_7};
    use lumix_exchange::{Exchange, ExchangeClient, MARKET_DECREASE, MARKET_INCREASE};
    use lumix_factory::{Factory, FactoryClient};
    use lumix_market::{Market, MarketClient};
    use lumix_oracle::{Oracle, OracleClient};
    use lumix_pool::{Pool, PoolClient};
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        token::StellarAssetClient,
        Address, Env, String,
    };

    fn mint(env: &Env, sac: &Address, to: &Address, amount: i128) {
        StellarAssetClient::new(env, sac).mint(to, &amount);
    }

    fn refresh_oracle(
        oracle: &OracleClient,
        admin: &Address,
        long: &Address,
        short: &Address,
        px: i128,
    ) {
        oracle.set_mid(admin, long, &px);
        oracle.set_mid(admin, short, &PRICE_SCALE);
    }

    #[test]
    fn deposit_open_accrue_close_withdraw() {
        let env = Env::default();
        env.cost_estimate().budget().reset_unlimited();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let lp = Address::generate(&env);
        let trader = Address::generate(&env);
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

        let fac_id = env.register(Factory, (admin.clone(),));
        FactoryClient::new(&env, &fac_id)
            .register_market(&market_id, &pool_id, &oracle_id, &long, &long, &short);
        exchange.set_factory(&fac_id);
        mint(&env, &long, &lp, 1_000_000 * SCALAR_7);
        mint(&env, &short, &lp, 1_000_000 * SCALAR_7);
        mint(&env, &short, &trader, 100_000 * SCALAR_7);

        let shares = pool.deposit(&lp, &(40_000 * SCALAR_7), &(20_000 * SCALAR_7), &0);
        assert!(shares > 0);
        let price_before = pool.share_price();

        let oid = exchange.create_order(
            &trader,
            &market_id,
            &MARKET_INCREASE,
            &true,
            &(1_000 * SCALAR_7),
            &(5_000 * SCALAR_7),
            &PRICE_SCALE,
            &1,
            &0,
            &true,
            &0u64,
            &1u32,
        );
        exchange.execute_order(&keeper, &oid);
        assert_eq!(market.get_oi().long_usd, 5_000 * SCALAR_7);

        env.ledger().set_timestamp(env.ledger().timestamp() + 3_600);
        refresh_oracle(&oracle, &admin, &long, &short, PRICE_SCALE / 2);

        let close = exchange.create_order(
            &trader,
            &market_id,
            &MARKET_DECREASE,
            &true,
            &0,
            &(5_000 * SCALAR_7),
            &1,
            &1,
            &0,
            &true,
            &0u64,
            &1u32,
        );
        exchange.execute_order(&keeper, &close);
        assert_eq!(market.get_oi().long_usd, 0);

        let (l, s) = pool.withdraw(&lp, &shares, &0, &0);
        assert!(l > 0 && s > 0);
        let _ = price_before;
    }

    #[test]
    fn pending_pnl_moves_with_price() {
        let env = Env::default();
        env.cost_estimate().budget().reset_unlimited();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let user = Address::generate(&env);
        let long_sac = env.register_stellar_asset_contract_v2(admin.clone());
        let short_sac = env.register_stellar_asset_contract_v2(admin.clone());
        let long = long_sac.address();
        let short = short_sac.address();
        let oracle_id = env.register(Oracle, (admin.clone(), 600u64, 20_000u32, 0u32));
        let oracle = OracleClient::new(&env, &oracle_id);
        oracle.set_mid(&admin, &long, &(PRICE_SCALE / 2));
        oracle.set_mid(&admin, &short, &PRICE_SCALE);
        let fee = Address::generate(&env);
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
                oracle_id,
                long.clone(),
                Address::generate(&env),
            ),
        );
        let market = MarketClient::new(&env, &market_id);
        pool.set_market(&market_id);
        let exchange = Address::generate(&env);
        market.set_exchange(&exchange);

        mint(&env, &long, &user, 1_000_000 * SCALAR_7);
        mint(&env, &short, &user, 1_000_000 * SCALAR_7);
        pool.deposit(&user, &(20_000 * SCALAR_7), &(10_000 * SCALAR_7), &0);
        soroban_sdk::token::TokenClient::new(&env, &short).transfer(
            &user,
            &pool.address,
            &(500 * SCALAR_7),
        );
        market.increase(
            &user,
            &true,
            &(500 * SCALAR_7),
            &(2_000 * SCALAR_7),
            &PRICE_SCALE,
            &true,
            &env.ledger().timestamp(),
        );
        env.ledger().set_timestamp(env.ledger().timestamp() + 1);
        oracle.set_mid(&admin, &short, &PRICE_SCALE);
        oracle.set_mid(&admin, &long, &PRICE_SCALE);
        let pnl = market.pending_pnl();
        assert!(pnl > 0);
    }

    #[test]
    fn solvency_reserved_blocks_full_withdraw() {
        let env = Env::default();
        env.cost_estimate().budget().reset_unlimited();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let lp = Address::generate(&env);
        let trader = Address::generate(&env);
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
                Address::generate(&env),
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
                oracle_id,
                long.clone(),
                Address::generate(&env),
            ),
        );
        pool.set_market(&market_id);
        let market = MarketClient::new(&env, &market_id);
        market.set_exchange(&Address::generate(&env));
        mint(&env, &long, &lp, 1_000_000 * SCALAR_7);
        mint(&env, &short, &lp, 1_000_000 * SCALAR_7);
        mint(&env, &short, &trader, 50_000 * SCALAR_7);
        let shares = pool.deposit(&lp, &(2_000 * SCALAR_7), &(1_000 * SCALAR_7), &0);
        soroban_sdk::token::TokenClient::new(&env, &short).transfer(
            &trader,
            &pool.address,
            &(200 * SCALAR_7),
        );
        market.increase(
            &trader,
            &true,
            &(200 * SCALAR_7),
            &(800 * SCALAR_7),
            &PRICE_SCALE,
            &true,
            &env.ledger().timestamp(),
        );
        assert!(pool.try_withdraw(&lp, &shares, &0, &0).is_err());
    }
}
