extern crate std;
use super::*;
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short,
    testutils::{Address as _, EnvTestConfig, Ledger, MockAuth, MockAuthInvoke},
    token::{Client as Token, StellarAssetClient},
    vec, IntoVal, Symbol,
};

#[contracttype]
#[derive(Clone)]
struct State {
    tokens: Vec<Address>,
    n: i128,
    d: i128,
    fail: bool,
    lie: bool,
    overpull: bool,
    protocol: Protocol,
    reserves: (i128, i128),
}
fn state(e: &Env) -> State {
    e.storage().instance().get(&symbol_short!("state")).unwrap()
}
fn cost(e: &Env, out: i128) -> i128 {
    let s = state(e);
    (out * s.d + s.n - 1) / s.n
}
fn exchange(e: &Env, user: &Address, input: &Address, amount: i128, out: i128) -> i128 {
    let s = state(e);
    assert!(!s.fail, "pool failed");
    user.require_auth();
    let output = s.tokens.iter().find(|t| t != input).unwrap();
    Token::new(e, input).transfer(
        user,
        e.current_contract_address(),
        &(amount + if s.overpull { 1 } else { 0 }),
    );
    Token::new(e, &output).transfer(
        &e.current_contract_address(),
        user,
        &(if s.lie { 1 } else { out }),
    );
    out
}
mod aqua {
    use super::*;
    #[contract]
    pub struct Pool;
    #[contractimpl]
    impl Pool {
        pub fn get_tokens(e: Env) -> Vec<Address> {
            state(&e).tokens
        }
        pub fn swap(e: Env, user: Address, i: u32, _j: u32, amount: u128, min: u128) -> u128 {
            let s = state(&e);
            let out = amount as i128 * s.n / s.d;
            assert!(out >= min as i128);
            exchange(&e, &user, &s.tokens.get(i).unwrap(), amount as i128, out) as u128
        }
        pub fn estimate_swap_strict_receive(e: Env, _i: u32, _j: u32, out: u128) -> u128 {
            cost(&e, out as i128) as u128
        }
        pub fn swap_strict_receive(
            e: Env,
            user: Address,
            i: u32,
            _j: u32,
            out: u128,
            max: u128,
        ) -> u128 {
            let amount = cost(&e, out as i128);
            assert!(amount <= max as i128);
            let input = state(&e).tokens.get(i).unwrap();
            exchange(&e, &user, &input, max as i128, out as i128);
            if max as i128 > amount {
                Token::new(&e, &input).transfer(
                    &e.current_contract_address(),
                    &user,
                    &(max as i128 - amount),
                );
            }
            amount as u128
        }
        pub fn pay(e: Env, to: Address, token: Address, amount: i128) {
            let s = state(&e);
            assert!(!s.fail);
            Token::new(&e, &token).transfer(
                &e.current_contract_address(),
                &to,
                &(if s.lie { 1 } else { amount }),
            );
        }
    }
}
// Match the deployed pool's eight-argument ABI, including generated clients.
#[allow(clippy::too_many_arguments)]
mod phoenix {
    use super::*;
    #[contract]
    pub struct Pool;
    #[contractimpl]
    impl Pool {
        pub fn query_config(e: Env) -> Map<Symbol, soroban_sdk::Val> {
            let s = state(&e);
            let mut cfg = Map::new(&e);
            cfg.set(
                symbol_short!("token_a"),
                s.tokens.get(0).unwrap().into_val(&e),
            );
            cfg.set(
                symbol_short!("token_b"),
                s.tokens.get(1).unwrap().into_val(&e),
            );
            cfg.set(Symbol::new(&e, "total_fee_bps"), 50i64.into_val(&e));
            cfg
        }
        pub fn swap(
            e: Env,
            user: Address,
            token: Address,
            amount: i128,
            min: Option<i128>,
            spread: Option<i64>,
            deadline: Option<u64>,
            fee: Option<i64>,
        ) -> i128 {
            assert!(deadline.unwrap() >= e.ledger().timestamp());
            assert!(fee.unwrap() >= 50);
            assert!(spread.unwrap_or(10000) >= 20);
            let s = state(&e);
            let out = amount * s.n / s.d;
            assert!(out >= min.unwrap());
            exchange(&e, &user, &token, amount, out)
        }
    }
}
mod sushi {
    use super::*;
    #[contracttype]
    #[derive(Clone)]
    pub struct Hints {
        pub checkpoint: u32,
        pub checkpoint_min: u32,
        pub slot: u128,
    }
    #[contract]
    pub struct Pool;
    #[contractimpl]
    impl Pool {
        pub fn token0(e: Env) -> Address {
            state(&e).tokens.get(0).unwrap()
        }
        pub fn token1(e: Env) -> Address {
            state(&e).tokens.get(1).unwrap()
        }
        pub fn get_oracle_hints(_e: Env) -> Hints {
            Hints {
                checkpoint: 0,
                checkpoint_min: 0,
                slot: 0,
            }
        }
        pub fn swap(
            e: Env,
            sender: Address,
            _recipient: Address,
            zero: bool,
            amount: i128,
            _price: soroban_sdk::U256,
            _hints: Hints,
        ) -> i128 {
            let s = state(&e);
            let input = s.tokens.get(if zero { 0 } else { 1 }).unwrap();
            exchange(&e, &sender, &input, amount, amount * s.n / s.d)
        }
    }
}
mod comet {
    use super::*;
    #[contract]
    pub struct Pool;
    #[allow(clippy::too_many_arguments)]
    fn pull(
        e: &Env,
        user: &Address,
        input: &Address,
        cap: i128,
        amount: i128,
        output: &Address,
        out: i128,
        price: i128,
    ) -> (i128, i128) {
        let s = state(e);
        assert!(!s.fail);
        assert!(price >= 100);
        user.require_auth();
        let expiration = (e.ledger().sequence() / 100000 + 1) * 100000;
        let pool = e.current_contract_address();
        Token::new(e, input).approve(user, &pool, &cap, &expiration);
        Token::new(e, input).transfer_from(&pool, user, &pool, &amount);
        Token::new(e, output).transfer(&pool, user, &out);
        (out, 100)
    }
    #[contractimpl]
    impl Pool {
        pub fn get_tokens(e: Env) -> Vec<Address> {
            state(&e).tokens
        }
        pub fn swap_exact_amount_in(
            e: Env,
            input: Address,
            amount: i128,
            output: Address,
            min: i128,
            price: i128,
            user: Address,
        ) -> (i128, i128) {
            let s = state(&e);
            let out = amount * s.n / s.d;
            assert!(out >= min);
            pull(&e, &user, &input, amount, amount, &output, out, price)
        }
        pub fn swap_exact_amount_out(
            e: Env,
            input: Address,
            cap: i128,
            output: Address,
            out: i128,
            price: i128,
            user: Address,
        ) -> (i128, i128) {
            let amount = cost(&e, out);
            assert!(amount <= cap);
            pull(&e, &user, &input, cap, amount, &output, out, price);
            (amount, 100)
        }
    }
}
mod soroswap {
    use super::*;
    #[contract]
    pub struct Pool;
    #[contractimpl]
    impl Pool {
        pub fn token_0(e: Env) -> Address {
            state(&e).tokens.get(0).unwrap()
        }
        pub fn token_1(e: Env) -> Address {
            state(&e).tokens.get(1).unwrap()
        }
        pub fn get_reserves(e: Env) -> (i128, i128) {
            state(&e).reserves
        }
        pub fn swap(e: Env, out0: i128, out1: i128, to: Address) {
            let mut s = state(&e);
            assert!(!s.fail);
            let owner = e.current_contract_address();
            let a = s.tokens.get(0).unwrap();
            let b = s.tokens.get(1).unwrap();
            assert!(out0 >= 0 && out1 >= 0 && out0 + out1 > 0);
            if out0 > 0 {
                Token::new(&e, &a).transfer(&owner, &to, &out0);
            }
            if out1 > 0 {
                Token::new(&e, &b).transfer(&owner, &to, &out1);
            }
            let balance0 = Token::new(&e, &a).balance(&owner);
            let balance1 = Token::new(&e, &b).balance(&owner);
            let input0 = (balance0 - (s.reserves.0 - out0)).max(0);
            let input1 = (balance1 - (s.reserves.1 - out1)).max(0);
            assert!(
                (balance0 - (input0 * 3 + 999) / 1000) * (balance1 - (input1 * 3 + 999) / 1000)
                    >= s.reserves.0 * s.reserves.1
            );
            s.reserves = (balance0, balance1);
            e.storage().instance().set(&symbol_short!("state"), &s);
        }
    }
}
struct Fixture {
    e: Env,
    agg: Address,
    user: Address,
    dst: Address,
    treasury: Address,
    a: Address,
    b: Address,
    c: Address,
}
impl Fixture {
    fn new(bps: Option<u32>) -> Self {
        let e = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        e.ledger().set_timestamp(100);
        e.mock_all_auths();
        let admin = Address::generate(&e);
        let user = Address::generate(&e);
        let dst = Address::generate(&e);
        let treasury = Address::generate(&e);
        let a = e
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let b = e
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        let c = e
            .register_stellar_asset_contract_v2(admin.clone())
            .address();
        for t in [&a, &b, &c] {
            StellarAssetClient::new(&e, t).mint(&user, &1_000_000);
        }
        let agg = register_aggregator(&e, &admin, &treasury, bps);
        Self {
            e,
            agg,
            user,
            dst,
            treasury,
            a,
            b,
            c,
        }
    }
    fn client(&self) -> AggregatorClient<'_> {
        AggregatorClient::new(&self.e, &self.agg)
    }
    fn pool(&self, protocol: Protocol, a: &Address, b: &Address) -> Address {
        // Registration/mints belong to fixture setup, not the swap's budget.
        self.e.cost_estimate().budget().reset_unlimited();
        self.e.mock_all_auths();
        let id = match protocol {
            Protocol::Soroswap => self.e.register(soroswap::Pool, ()),
            Protocol::Phoenix => self.e.register(phoenix::Pool, ()),
            Protocol::Comet => self.e.register(comet::Pool, ()),
            Protocol::Sushi => self.e.register(sushi::Pool, ()),
            _ => self.e.register(aqua::Pool, ()),
        };
        let tokens = vec![&self.e, a.clone(), b.clone()];
        let s = State {
            tokens: tokens.clone(),
            n: 2,
            d: 1,
            fail: false,
            lie: false,
            overpull: false,
            protocol: protocol.clone(),
            reserves: (10_000_000, 20_000_000),
        };
        self.e.as_contract(&id, || {
            self.e.storage().instance().set(&symbol_short!("state"), &s)
        });
        for t in [a, b] {
            StellarAssetClient::new(&self.e, t).mint(&id, &10_000_000);
        }
        if protocol == Protocol::Soroswap {
            StellarAssetClient::new(&self.e, b).mint(&id, &10_000_000);
        }
        id
    }
    fn hop(&self, pool: &Address, a: &Address, b: &Address, output: i128, limit: i128) -> Hop {
        Hop {
            protocol: self.e.as_contract(pool, || state(&self.e).protocol),
            pool: pool.clone(),
            token_in: a.clone(),
            token_out: b.clone(),
            output,
            limit,
            max_spread_bps: None,
            max_price: None,
        }
    }
    fn request(&self, pool: &Address, exact_out: bool) -> SwapRequest {
        let gross = gross_for_net(180, self.client().get_config().fee_bps).unwrap();
        let hop = self.hop(
            pool,
            &self.a,
            &self.b,
            if exact_out { gross } else { 0 },
            if exact_out { 110 } else { 180 },
        );
        SwapRequest {
            sender: self.user.clone(),
            recipient: None,
            token_in: self.a.clone(),
            token_out: self.b.clone(),
            amount: if exact_out { 180 } else { 100 },
            limit: if exact_out {
                110
            } else {
                180 - fee(180, self.client().get_config().fee_bps).unwrap()
            },
            deadline: 200,
            fee_bps: self.client().get_config().fee_bps,
            legs: vec![
                &self.e,
                Leg {
                    amount: if exact_out { gross } else { 100 },
                    limit: if exact_out { 110 } else { 180 },
                    hops: vec![&self.e, hop],
                },
            ],
        }
    }
    fn auth(&self, r: &SwapRequest, out: bool) {
        self.e.cost_estimate().budget().reset_unlimited();
        let funding: i128 = if out {
            r.legs.iter().map(|l| l.limit).sum()
        } else {
            r.amount
        };
        self.e.mock_auths(&[MockAuth {
            address: &self.user,
            invoke: &MockAuthInvoke {
                contract: &self.agg,
                fn_name: if out {
                    "swap_exact_out"
                } else {
                    "swap_exact_in"
                },
                args: (r.clone(),).into_val(&self.e),
                sub_invokes: &[MockAuthInvoke {
                    contract: &self.a,
                    fn_name: "transfer",
                    args: (self.user.clone(), self.agg.clone(), funding).into_val(&self.e),
                    sub_invokes: &[],
                }],
            },
        }]);
        // Installing the mock signer contract is fixture setup, not swap work.
        self.e.cost_estimate().budget().reset_default();
    }
    fn mutate(&self, p: &Address, f: impl FnOnce(&mut State)) {
        self.e.as_contract(p, || {
            let mut s = state(&self.e);
            f(&mut s);
            self.e.storage().instance().set(&symbol_short!("state"), &s);
        });
    }
    fn bal(&self, t: &Address, a: &Address) -> i128 {
        Token::new(&self.e, t).balance(a)
    }
}
fn register_aggregator(e: &Env, admin: &Address, treasury: &Address, fee: Option<u32>) -> Address {
    #[cfg(not(feature = "wasm-tests"))]
    {
        e.register(Aggregator, (admin.clone(), treasury.clone(), fee))
    }
    #[cfg(feature = "wasm-tests")]
    {
        e.register(
            include_bytes!("../target/wasm32v1-none/release/lumix_aggregator.wasm").as_slice(),
            (admin.clone(), treasury.clone(), fee),
        )
    }
}

#[test]
fn each_protocol_both_modes_strict_auth_and_real_tokens() {
    for protocol in [
        Protocol::Soroswap,
        Protocol::Aquarius,
        Protocol::Phoenix,
        Protocol::Sushi,
        Protocol::Comet,
    ] {
        for out in [false, true] {
            let f = Fixture::new(None);
            let p = f.pool(protocol.clone(), &f.a, &f.b);
            let mut r = f.request(&p, out);
            r.recipient = Some(f.dst.clone());
            f.auth(&r, out);
            let res = if out {
                f.client().swap_exact_out(&r)
            } else {
                f.client().swap_exact_in(&r)
            };
            let fallback = protocol == Protocol::Phoenix || protocol == Protocol::Sushi;
            assert_eq!(
                res.amount_out,
                if out {
                    180
                } else if protocol == Protocol::Soroswap {
                    197
                } else {
                    200
                }
            );
            assert_eq!(
                res.amount_in,
                if out {
                    if fallback {
                        110
                    } else if protocol == Protocol::Soroswap {
                        92
                    } else {
                        90
                    }
                } else {
                    100
                }
            );
            assert_eq!(f.bal(&f.b, &f.dst), res.amount_out);
            assert_eq!(f.bal(&f.a, &f.user), 1_000_000 - res.amount_in);
            assert_eq!(f.bal(&f.b, &f.user), 1_000_000 + res.surplus_out);
            assert_eq!(f.bal(&f.a, &f.agg), 0);
            assert_eq!(f.bal(&f.b, &f.agg), 0);
            assert_eq!(Token::new(&f.e, &f.a).allowance(&f.agg, &p), 0);
        }
    }
}
#[test]
fn fees_and_donated_balances_are_preserved() {
    for out in [false, true] {
        let f = Fixture::new(Some(100));
        let p = f.pool(Protocol::Comet, &f.a, &f.b);
        let r = f.request(&p, out);
        for t in [&f.a, &f.b] {
            StellarAssetClient::new(&f.e, t).mint(&f.agg, &77);
        }
        f.auth(&r, out);
        let res = if out {
            f.client().swap_exact_out(&r)
        } else {
            f.client().swap_exact_in(&r)
        };
        assert_eq!(res.fee, if out { 1 } else { 2 });
        assert_eq!(f.bal(&f.b, &f.treasury), res.fee);
        assert_eq!(f.bal(&f.a, &f.agg), 77);
        assert_eq!(f.bal(&f.b, &f.agg), 77);
    }
}
#[test]
fn mixed_protocol_multihop_and_split_both_modes() {
    for out in [false, true] {
        let f = Fixture::new(None);
        let p = f.pool(Protocol::Phoenix, &f.a, &f.c);
        let q = f.pool(Protocol::Comet, &f.c, &f.b);
        let direct = f.pool(Protocol::Soroswap, &f.a, &f.b);
        let mut r = f.request(&direct, out);
        r.amount = if out { 280 } else { 150 };
        r.limit = if out { 170 } else { 280 };
        r.legs.push_back(Leg {
            amount: if out { 100 } else { 50 },
            limit: if out { 60 } else { 100 },
            hops: vec![
                &f.e,
                f.hop(
                    &p,
                    &f.a,
                    &f.c,
                    if out { 60 } else { 0 },
                    if out { 60 } else { 80 },
                ),
                f.hop(
                    &q,
                    &f.c,
                    &f.b,
                    if out { 100 } else { 0 },
                    if out { 60 } else { 100 },
                ),
            ],
        });
        f.auth(&r, out);
        // Raise the SDK diagnostic auth-snapshot cap; verify execution usage below.
        f.e.cost_estimate()
            .budget()
            .reset_limits(1_000_000_000, 512 * 1024 * 1024);
        let res = if out {
            f.client().swap_exact_out(&r)
        } else {
            f.client().swap_exact_in(&r)
        };
        assert!(f.e.cost_estimate().budget().cpu_instruction_cost() < 100_000_000);
        assert!(f.e.cost_estimate().budget().memory_bytes_cost() < 40 * 1024 * 1024);
        assert_eq!(res.amount_out, if out { 280 } else { 397 });
        assert_eq!(res.amount_in, if out { 152 } else { 150 });
        assert_eq!(f.bal(&f.c, &f.user), 1_000_000 + if out { 70 } else { 0 });
        for t in [&f.a, &f.b, &f.c] {
            assert_eq!(f.bal(t, &f.agg), 0);
        }
    }
}
#[test]
fn later_leg_failure_rolls_back_every_balance_and_allowance() {
    for out in [false, true] {
        let f = Fixture::new(Some(100));
        let p = f.pool(Protocol::Comet, &f.a, &f.b);
        let q = f.pool(Protocol::Aquarius, &f.a, &f.b);
        let mut r = f.request(&p, out);
        let mut second = r.legs.get(0).unwrap();
        second.hops.set(
            0,
            f.hop(
                &q,
                &f.a,
                &f.b,
                if out { second.amount } else { 0 },
                if out { second.limit } else { 180 },
            ),
        );
        r.legs.push_back(second);
        r.amount *= 2;
        r.limit *= 2;
        if out {
            let mut leg = r.legs.get(1).unwrap();
            leg.amount = gross_for_net(r.amount, 100).unwrap() - r.legs.get(0).unwrap().amount;
            let mut h = leg.hops.get(0).unwrap();
            h.output = leg.amount;
            leg.hops.set(0, h);
            r.legs.set(1, leg);
        }
        f.mutate(&q, |s| s.fail = true);
        f.auth(&r, out);
        assert!(if out {
            f.client().try_swap_exact_out(&r).is_err()
        } else {
            f.client().try_swap_exact_in(&r).is_err()
        });
        assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
        assert_eq!(f.bal(&f.b, &f.user), 1_000_000);
        for t in [&f.a, &f.b] {
            assert_eq!(f.bal(t, &f.agg), 0);
            assert_eq!(f.bal(t, &f.treasury), 0);
        }
        assert_eq!(f.bal(&f.a, &p), 10_000_000);
        assert_eq!(Token::new(&f.e, &f.a).allowance(&f.agg, &p), 0);
    }
}
#[test]
fn lying_pool_cannot_satisfy_minimum_with_a_return_value_or_donation() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let r = f.request(&p, false);
    StellarAssetClient::new(&f.e, &f.b).mint(&f.agg, &1000);
    f.mutate(&p, |s| s.lie = true);
    f.auth(&r, false);
    assert_eq!(
        f.client().try_swap_exact_in(&r),
        Err(Ok(Error::MinimumNotMet))
    );
    assert_eq!(f.bal(&f.b, &f.agg), 1000);
    assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
}
#[test]
fn rejects_bad_requests_before_funding() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let good = f.request(&p, false);
    for kind in 0..12 {
        let mut r = good.clone();
        let mut leg = r.legs.get(0).unwrap();
        let mut h = leg.hops.get(0).unwrap();
        match kind {
            0 => r.amount = 0,
            1 => r.limit = -1,
            2 => r.fee_bps = 1,
            3 => r.deadline = 99,
            4 => r.amount += 1,
            5 => r.limit = 181,
            6 => h.token_in = f.c.clone(),
            7 => h.pool = Address::generate(&f.e),
            8 => r.recipient = Some(f.agg.clone()),
            9 => h.output = 1,
            10 => h.max_spread_bps = Some(10001),
            11 => h.max_price = Some(0),
            _ => (),
        }
        leg.hops.set(0, h);
        r.legs.set(0, leg);
        f.auth(&r, false);
        assert!(f.client().try_swap_exact_in(&r).is_err());
        assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
    }
    let mut r = good;
    r.legs.push_back(r.legs.get(0).unwrap());
    r.amount *= 2;
    r.limit *= 2;
    f.auth(&r, false);
    assert_eq!(
        f.client().try_swap_exact_in(&r),
        Err(Ok(Error::InvalidRoute))
    );
}
#[test]
fn exact_out_caps_targets_and_intermediate_funding_are_consistent() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let good = f.request(&p, true);
    for kind in 0..4 {
        let mut r = good.clone();
        let mut l = r.legs.get(0).unwrap();
        let mut h = l.hops.get(0).unwrap();
        match kind {
            0 => r.limit = 109,
            1 => l.amount += 1,
            2 => h.limit = 111,
            _ => h.output = 181,
        }
        l.hops.set(0, h);
        r.legs.set(0, l);
        f.auth(&r, true);
        assert_eq!(
            f.client().try_swap_exact_out(&r),
            Err(Ok(Error::InconsistentTotals))
        );
    }
    let mut r = good;
    let mut l = r.legs.get(0).unwrap();
    l.limit = 80;
    let mut h = l.hops.get(0).unwrap();
    h.limit = 80;
    l.hops.set(0, h);
    r.legs.set(0, l);
    f.auth(&r, true);
    assert_eq!(
        f.client().try_swap_exact_out(&r),
        Err(Ok(Error::MaximumExceeded))
    );
    assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
}
#[test]
fn signatures_bind_the_destination_and_admin_changes_need_admin() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let mut r = f.request(&p, false);
    f.e.mock_auths(&[]);
    assert!(f.client().try_swap_exact_in(&r).is_err());
    assert!(f.client().try_set_fee(&100, &f.treasury).is_err());
    f.auth(&r, false);
    r.recipient = Some(f.dst.clone());
    assert!(f.client().try_swap_exact_in(&r).is_err());
    f.e.mock_all_auths();
    f.client().set_fee(&100, &f.treasury);
    f.auth(&r, false);
    assert_eq!(f.client().try_swap_exact_in(&r), Err(Ok(Error::FeeChanged)));
}
#[test]
fn configuration_expiry_and_bounds() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    assert_eq!(f.client().get_config().fee_bps, 0);
    assert_eq!(
        f.client().try_set_fee(&10000, &f.treasury),
        Err(Ok(Error::InvalidFee))
    );
    let mut r = f.request(&p, false);
    r.deadline = 100;
    f.auth(&r, false);
    f.client().swap_exact_in(&r);
    r.legs = vec![&f.e];
    f.auth(&r, false);
    assert_eq!(
        f.client().try_swap_exact_in(&r),
        Err(Ok(Error::InvalidRoute))
    );
    r = f.request(&p, false);
    for _ in 0..8 {
        r.legs.push_back(r.legs.get(0).unwrap());
    }
    f.auth(&r, false);
    assert_eq!(
        f.client().try_swap_exact_in(&r),
        Err(Ok(Error::TooManySteps))
    );
}
#[test]
fn limits_from_protocols_are_forwarded() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Phoenix, &f.a, &f.b);
    let mut r = f.request(&p, false);
    let mut l = r.legs.get(0).unwrap();
    let mut h = l.hops.get(0).unwrap();
    h.max_spread_bps = Some(19);
    l.hops.set(0, h);
    r.legs.set(0, l);
    f.auth(&r, false);
    assert!(f.client().try_swap_exact_in(&r).is_err());
    let p = f.pool(Protocol::Comet, &f.a, &f.b);
    let mut r = f.request(&p, false);
    let mut l = r.legs.get(0).unwrap();
    let mut h = l.hops.get(0).unwrap();
    h.max_price = Some(99);
    l.hops.set(0, h);
    r.legs.set(0, l);
    f.auth(&r, false);
    assert!(f.client().try_swap_exact_in(&r).is_err());
}
proptest::proptest! {
    #[test]
    fn fee_rounding_and_exact_out_gross_are_minimal(n in 1i128..i128::MAX/10000, bps in 0u32..10000) {
        let gross = gross_for_net(n,bps).unwrap();
        proptest::prop_assert_eq!(gross-fee(gross,bps).unwrap(),n);
        proptest::prop_assert!(gross==1 || gross-1-fee(gross-1,bps).unwrap()<n);
    }
}
#[test]
fn overflow_does_not_wrap() {
    assert_eq!(
        fee(i128::MAX, 9999).unwrap(),
        i128::MAX / 10000 * 9999 + i128::MAX % 10000 * 9999 / 10000
    );
    assert_eq!(gross_for_net(i128::MAX, 1), Err(Error::Overflow));
}

#[test]
fn missing_classic_destination_account_reverts_the_completed_pool_swap() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let mut req = f.request(&p, false);
    req.recipient = Some(Address::from_str(
        &f.e,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    f.auth(&req, false);
    assert!(f.client().try_swap_exact_in(&req).is_err());
    assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
    assert_eq!(f.bal(&f.b, &f.user), 1_000_000);
    assert_eq!(f.bal(&f.a, &p), 10_000_000);
    assert_eq!(f.bal(&f.b, &p), 10_000_000);
}

#[test]
fn javascript_xdr_decodes_to_the_rust_request_abi() {
    use soroban_sdk::xdr::{Limits, ReadXdr, ScVal};
    use soroban_sdk::{TryFromVal, Val};
    let e = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/abi-requests.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        let wire =
            ScVal::from_xdr_base64(fixture["xdr"].as_str().unwrap(), Limits::none()).unwrap();
        let value = Val::try_from_val(&e, &wire).unwrap();
        let req = SwapRequest::try_from_val(&e, &value).unwrap();
        let out = fixture["mode"] == "exact-out";
        assert_eq!(req.amount, 100);
        assert_eq!(req.limit, if out { 120 } else { 90 });
        assert_eq!(req.deadline, 1800000000);
        assert_eq!(req.legs.len(), 1);
        assert_eq!(req.legs.get(0).unwrap().hops.len(), 2);
        assert_eq!(
            req.legs.get(0).unwrap().hops.get(0).unwrap().protocol,
            Protocol::Soroswap
        );
        assert_eq!(
            req.legs.get(0).unwrap().hops.get(1).unwrap().protocol,
            Protocol::Comet
        );
        assert_eq!(
            req.legs.get(0).unwrap().hops.get(1).unwrap().max_price,
            Some(123456789)
        );
        assert_eq!(
            req.legs.get(0).unwrap().hops.get(0).unwrap().max_spread_bps,
            Some(100)
        );
        assert_eq!(req.recipient.is_some(), out);
    }
}

#[test]
fn pool_metadata_and_protocol_are_checked_without_registration() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let good = f.request(&p, false);
    for protocol in [
        Protocol::Soroswap,
        Protocol::Phoenix,
        Protocol::Sushi,
        Protocol::Comet,
    ] {
        let mut r = good.clone();
        let mut leg = r.legs.get(0).unwrap();
        let mut hop = leg.hops.get(0).unwrap();
        hop.protocol = protocol;
        leg.hops.set(0, hop);
        r.legs.set(0, leg);
        f.auth(&r, false);
        assert!(f.client().try_swap_exact_in(&r).is_err());
        assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
    }
    for tokens in [
        vec![&f.e, f.a.clone(), f.c.clone()],
        vec![&f.e, f.a.clone(), f.b.clone(), f.a.clone()],
    ] {
        f.mutate(&p, |s| s.tokens = tokens);
        f.auth(&good, false);
        assert_eq!(
            f.client().try_swap_exact_in(&good),
            Err(Ok(Error::InvalidPool))
        );
        assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
    }
}

#[test]
fn changing_the_protocol_invalidates_the_sender_signature() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let mut r = f.request(&p, false);
    f.auth(&r, false);
    let mut leg = r.legs.get(0).unwrap();
    let mut hop = leg.hops.get(0).unwrap();
    hop.protocol = Protocol::Comet;
    leg.hops.set(0, hop);
    r.legs.set(0, leg);
    assert!(f.client().try_swap_exact_in(&r).is_err());
    assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
}

#[test]
fn arbitrary_pool_cannot_pull_more_than_the_signed_hop_budget() {
    let f = Fixture::new(None);
    let p = f.pool(Protocol::Aquarius, &f.a, &f.b);
    let r = f.request(&p, false);
    StellarAssetClient::new(&f.e, &f.a).mint(&f.agg, &1000);
    f.mutate(&p, |s| s.overpull = true);
    f.auth(&r, false);
    assert!(f.client().try_swap_exact_in(&r).is_err());
    assert_eq!(f.bal(&f.a, &f.agg), 1000);
    assert_eq!(f.bal(&f.a, &f.user), 1_000_000);
}

proptest::proptest! {
    #[test]
    fn soroswap_direct_quote_preserves_invariant_and_exact_out_is_minimal(
        rin in 1i128..1_000_000_000_000, rout in 2i128..1_000_000_000_000,
        input in 1i128..1_000_000_000, fraction in 1i128..10000,
    ) {
        let e = Env::new_with_config(EnvTestConfig { capture_snapshot_at_drop: false });
        let output = adapters::soroswap_amount(&e, rin, rout, input, false).unwrap();
        let retained = input - (input * 3 + 999) / 1000;
        proptest::prop_assert!((rin + retained) * (rout - output) >= rin * rout);
        proptest::prop_assert!((rin + retained) * (rout - output - 1) < rin * rout);
        let target = (rout * fraction / 10000).max(1);
        let required = adapters::soroswap_amount(&e, rin, rout, target, true).unwrap();
        proptest::prop_assert!(adapters::soroswap_amount(&e, rin, rout, required, false).unwrap() >= target);
        if required > 1 {
            proptest::prop_assert!(adapters::soroswap_amount(&e, rin, rout, required - 1, false).unwrap() < target);
        }
    }
}

#[test]
fn soroswap_wide_math_handles_large_amounts_and_rejects_exhausted_reserves() {
    let e = Env::default();
    assert!(adapters::soroswap_amount(&e, 1 << 100, 1 << 110, 1 << 90, false).unwrap() > 0);
    assert!(adapters::soroswap_amount(&e, 1 << 100, 1 << 110, 1 << 90, true).unwrap() > 0);
    assert_eq!(
        adapters::soroswap_amount(&e, 10, 20, 20, true),
        Err(Error::MaximumExceeded)
    );
    assert_eq!(
        adapters::soroswap_amount(&e, i128::MAX, i128::MAX, i128::MAX - 1, true),
        Err(Error::Overflow)
    );
}
