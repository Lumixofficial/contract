//! Shared native/Wasm fixtures and bounded state-machine fuzz scenarios.
use lumix_core::{PRICE_SCALE, SCALAR_7};
use lumix_exchange::{ExchangeClient, MARKET_INCREASE};
use lumix_market::MarketClient;
use lumix_options::{OptionKind, OptionsClient, OrderRequest, OrderSide};
use lumix_oracle::OracleClient;
use lumix_pool::PoolClient;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env, String,
};

pub const START: u64 = 1_000;
pub const EXPIRY: u64 = START + 86_400;
pub const INITIAL: i128 = 1_000_000 * SCALAR_7;

macro_rules! register {
    ($env:expr, $native:path, $wasm:literal, $args:expr) => {{
        #[cfg(not(feature = "wasm-tests"))]
        let id = $env.register($native, $args);
        #[cfg(feature = "wasm-tests")]
        let id = $env.register(
            &include_bytes!(concat!("../../target/wasm32v1-none/release/", $wasm))[..],
            $args,
        );
        id
    }};
}

pub struct Fixture {
    pub env: Env,
    pub admin: Address,
    pub keeper: Address,
    pub alice: Address,
    pub bob: Address,
    pub lp: Address,
    pub treasury: Address,
    pub base: Address,
    pub quote: Address,
    pub oracle: OracleClient<'static>,
    pub pool: PoolClient<'static>,
    pub market: MarketClient<'static>,
    pub exchange: ExchangeClient<'static>,
    pub options: OptionsClient<'static>,
    pub factory: lumix_factory::FactoryClient<'static>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new(true)
    }
}

impl Fixture {
    pub fn new(fixed_quote: bool) -> Self {
        let mut env = Env::default();
        env.cost_estimate().budget().reset_unlimited();
        env.set_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();
        env.ledger().set_timestamp(START);
        let admin = Address::generate(&env);
        let keeper = Address::generate(&env);
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);
        let lp = Address::generate(&env);
        let treasury = Address::generate(&env);
        let base = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let quote = env
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        for trader in [&alice, &bob, &lp] {
            StellarAssetClient::new(&env, &base).mint(trader, &INITIAL);
            StellarAssetClient::new(&env, &quote).mint(trader, &INITIAL);
        }
        let oid = register!(
            env,
            lumix_oracle::Oracle,
            "lumix_oracle.wasm",
            (admin.clone(), 600u64, 20_000u32, 2u32)
        );
        let oracle = OracleClient::new(&env, &oid);
        oracle.set_keeper(&keeper, &true);
        if fixed_quote {
            oracle.set_fixed_price(&quote, &PRICE_SCALE);
        } else {
            oracle.set_price_at(&admin, &quote, &START, &PRICE_SCALE, &PRICE_SCALE);
        }
        oracle.set_price_at(&admin, &base, &START, &PRICE_SCALE, &PRICE_SCALE);
        let pid = register!(
            env,
            lumix_pool::Pool,
            "lumix_pool.wasm",
            (
                admin.clone(),
                base.clone(),
                quote.clone(),
                oid.clone(),
                treasury.clone(),
                String::from_str(&env, "GM"),
                String::from_str(&env, "GM")
            )
        );
        let pool = PoolClient::new(&env, &pid);
        let mid = register!(
            env,
            lumix_market::Market,
            "lumix_market.wasm",
            (
                admin.clone(),
                pid.clone(),
                oid.clone(),
                base.clone(),
                treasury.clone()
            )
        );
        let market = MarketClient::new(&env, &mid);
        pool.set_market(&mid);
        market.set_keeper(&keeper, &true);
        let eid = register!(
            env,
            lumix_exchange::Exchange,
            "lumix_exchange.wasm",
            (admin.clone(), oid.clone(), quote.clone())
        );
        let exchange = ExchangeClient::new(&env, &eid);
        market.set_exchange(&eid);
        exchange.set_keeper(&keeper, &true);
        let opt = register!(
            env,
            lumix_options::Options,
            "lumix_options.wasm",
            (admin.clone(), oid.clone())
        );
        let options = OptionsClient::new(&env, &opt);
        options.set_keeper(&keeper, &true);
        options.set_pair(&base, &quote, &true);
        options.set_expiry(&base, &quote, &EXPIRY, &true);
        let fid = register!(
            env,
            lumix_factory::Factory,
            "lumix_factory.wasm",
            (admin.clone(),)
        );
        let factory = lumix_factory::FactoryClient::new(&env, &fid);
        factory.register_market(&mid, &pid, &oid, &base, &base, &quote);
        exchange.set_factory(&fid);
        pool.deposit(&lp, &(100_000 * SCALAR_7), &(100_000 * SCALAR_7), &0);
        env.ledger().set_timestamp(START + 1);
        env.cost_estimate().budget().reset_unlimited();
        Self {
            env,
            admin,
            keeper,
            alice,
            bob,
            lp,
            treasury,
            base,
            quote,
            oracle,
            pool,
            market,
            exchange,
            options,
            factory,
        }
    }
}

impl Fixture {
    pub fn time(&self, timestamp: u64) {
        self.env.ledger().set_timestamp(timestamp);
        self.env.cost_estimate().budget().reset_unlimited();
    }
    pub fn publish(&self, timestamp: u64, price: i128) {
        self.oracle
            .set_price_at(&self.keeper, &self.base, &timestamp, &price, &price);
    }
    pub fn order(
        &self,
        trader: &Address,
        kind: u32,
        long: bool,
        collateral: i128,
        size: i128,
        fee: i128,
    ) -> u32 {
        let acceptable = if long == (kind == MARKET_INCREASE) {
            100 * PRICE_SCALE
        } else {
            1
        };
        self.exchange.create_order(
            trader,
            &self.market.address,
            &kind,
            &long,
            &collateral,
            &size,
            &acceptable,
            &1,
            &fee,
            &true,
            &0,
            &1,
        )
    }
    pub fn balance(&self, asset: &Address, who: &Address) -> i128 {
        TokenClient::new(&self.env, asset).balance(who)
    }
    pub fn option(
        &self,
        trader: &Address,
        side: OrderSide,
        kind: OptionKind,
        amount: i128,
        strike: i128,
        premium: i128,
    ) -> u32 {
        self.options.create_order(
            trader,
            &OrderRequest {
                amount,
                base: self.base.clone(),
                expiry: EXPIRY,
                kind,
                premium,
                quote: self.quote.clone(),
                strike,
                side,
            },
        )
    }
    pub fn assert_conservation(&self) {
        for asset in [&self.base, &self.quote] {
            let total: i128 = [
                &self.alice,
                &self.bob,
                &self.lp,
                &self.treasury,
                &self.keeper,
                &self.pool.address,
                &self.market.address,
                &self.exchange.address,
                &self.options.address,
            ]
            .into_iter()
            .map(|a| self.balance(asset, a))
            .sum();
            assert_eq!(total, 3 * INITIAL);
        }
    }
}
