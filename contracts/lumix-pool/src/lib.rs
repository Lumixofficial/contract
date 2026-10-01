#![no_std]

mod share_token;

use lumix_core::{
    amounts_for_shares, bps_of_ceil, can_reserve, cap_pending_pnl, pool_value_usd,
    price_impact_usd, shares_to_mint, split_protocol_fee, to_usd, usd_for_shares,
    DEFAULT_ATOMIC_SWAP_FEE_BPS, DEFAULT_MAX_PNL_FACTOR_BPS, DEFAULT_RESERVE_FACTOR_BPS,
    DEFAULT_SWAP_FEE_BALANCED_BPS, DEFAULT_SWAP_FEE_IMBALANCED_BPS, INSTANCE_TTL_EXTEND_TO,
    INSTANCE_TTL_THRESHOLD, SCALAR_7,
};
use lumix_interfaces::OracleContractClient;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, token,
    Address, Env, String,
};

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    Paused = 2,
    InvalidAmount = 3,
    Slippage = 4,
    InsufficientLiquidity = 5,
    CapExceeded = 6,
    Math = 7,
    DepositsDisabled = 8,
    SwapsDisabled = 9,
    BadToken = 10,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub admin: Address,
    pub market: Option<Address>,
    pub long_token: Address,
    pub short_token: Address,
    pub oracle: Address,
    pub fee_receiver: Address,
    pub deposits_enabled: bool,
    pub swaps_enabled: bool,
    pub paused: bool,
    pub reserve_factor_bps: u32,
    pub swap_fee_balanced_bps: u32,
    pub swap_fee_imbalanced_bps: u32,
    pub atomic_swap_fee_bps: u32,
    pub protocol_fee_bps: u32,
    pub max_pool_usd: i128,
    pub max_pnl_factor_bps: u32,
    pub impact_factor_bps: u32,
    pub impact_cap_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct PoolState {
    pub reserved_long: i128,
    pub reserved_short: i128,
    pub total_shares: i128,
    pub pending_trader_pnl: i128,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    State,
    ShareName,
    ShareSymbol,
    Collateral,
}

#[contractevent]
pub struct Deposited {
    pub user: Address,
    pub long_amount: i128,
    pub short_amount: i128,
    pub shares: i128,
}

#[contractevent]
pub struct Withdrawn {
    pub user: Address,
    pub shares: i128,
    pub long_amount: i128,
    pub short_amount: i128,
}

#[contractevent]
pub struct Swapped {
    pub user: Address,
    pub token_in: Address,
    pub amount_in: i128,
    pub amount_out: i128,
    pub fee: i128,
    pub atomic: bool,
}

#[contract]
pub struct Pool;

fn cfg(env: &Env) -> PoolConfig {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized))
}

fn state(env: &Env) -> PoolState {
    env.storage()
        .instance()
        .get(&DataKey::State)
        .unwrap_or(PoolState {
            reserved_long: 0,
            reserved_short: 0,
            total_shares: 0,
            pending_trader_pnl: 0,
        })
}

fn save_cfg(env: &Env, c: &PoolConfig) {
    env.storage().instance().set(&DataKey::Config, c);
}

fn save_state(env: &Env, s: &PoolState) {
    env.storage().instance().set(&DataKey::State, s);
}

fn require_admin(_env: &Env, c: &PoolConfig) {
    c.admin.require_auth();
}

fn require_market(env: &Env, c: &PoolConfig) {
    let m = c
        .market
        .clone()
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized));
    m.require_auth();
}

fn token_bal(env: &Env, token: &Address) -> i128 {
    let balance = token::TokenClient::new(env, token).balance(&env.current_contract_address());
    let c = cfg(env);
    if *token == c.short_token {
        balance
            .checked_sub(
                env.storage()
                    .instance()
                    .get::<_, i128>(&DataKey::Collateral)
                    .unwrap_or(0),
            )
            .unwrap_or_else(|| panic_with_error!(env, Error::Math))
    } else {
        balance
    }
}

fn transfer_in(env: &Env, token: &Address, from: &Address, amount: i128) {
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    token::TokenClient::new(env, token).transfer(from, env.current_contract_address(), &amount);
}

fn transfer_out(env: &Env, token: &Address, to: &Address, amount: i128) {
    if amount < 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    if amount == 0 {
        return;
    }
    if amount > token_bal(env, token) {
        panic_with_error!(env, Error::InsufficientLiquidity);
    }
    token::TokenClient::new(env, token).transfer(&env.current_contract_address(), to, &amount);
}

fn quote_usd(env: &Env, c: &PoolConfig, token: &Address, amount: i128) -> i128 {
    let oracle = OracleContractClient::new(env, &c.oracle);
    let p = oracle.quote(token);
    to_usd(amount, p.mid()).unwrap_or_else(|_| panic_with_error!(env, Error::Math))
}

fn pool_usd(env: &Env, c: &PoolConfig, s: &PoolState) -> i128 {
    let long = token_bal(env, &c.long_token);
    let short = token_bal(env, &c.short_token);
    let long_usd = quote_usd(env, c, &c.long_token, long);
    let short_usd = quote_usd(env, c, &c.short_token, short);
    let pnl = cap_pending_pnl(
        s.pending_trader_pnl,
        long_usd + short_usd,
        c.max_pnl_factor_bps,
    )
    .unwrap_or(s.pending_trader_pnl);
    pool_value_usd(long_usd, short_usd, pnl).unwrap_or_else(|_| panic_with_error!(env, Error::Math))
}

fn is_long_token(c: &PoolConfig, token: &Address) -> bool {
    *token == c.long_token
}

#[contractimpl]
impl Pool {
    pub fn __constructor(
        env: Env,
        admin: Address,
        long_token: Address,
        short_token: Address,
        oracle: Address,
        fee_receiver: Address,
        share_name: String,
        share_symbol: String,
    ) {
        if long_token == short_token {
            panic_with_error!(&env, Error::BadToken);
        }
        let c = PoolConfig {
            admin,
            market: None,
            long_token,
            short_token,
            oracle,
            fee_receiver,
            deposits_enabled: true,
            swaps_enabled: true,
            paused: false,
            reserve_factor_bps: DEFAULT_RESERVE_FACTOR_BPS,
            swap_fee_balanced_bps: DEFAULT_SWAP_FEE_BALANCED_BPS,
            swap_fee_imbalanced_bps: DEFAULT_SWAP_FEE_IMBALANCED_BPS,
            atomic_swap_fee_bps: DEFAULT_ATOMIC_SWAP_FEE_BPS,
            protocol_fee_bps: 2_000,
            max_pool_usd: 0,
            max_pnl_factor_bps: DEFAULT_MAX_PNL_FACTOR_BPS,
            impact_factor_bps: 10,
            impact_cap_bps: 50,
        };
        save_cfg(&env, &c);
        save_state(
            &env,
            &PoolState {
                reserved_long: 0,
                reserved_short: 0,
                total_shares: 0,
                pending_trader_pnl: 0,
            },
        );
        env.storage()
            .instance()
            .set(&DataKey::ShareName, &share_name);
        env.storage()
            .instance()
            .set(&DataKey::ShareSymbol, &share_symbol);
        bump(&env);
    }

    pub fn set_market(env: Env, market: Address) {
        let mut c = cfg(&env);
        require_admin(&env, &c);
        c.market = Some(market);
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn set_status(env: Env, deposits: bool, swaps: bool, paused: bool) {
        let mut c = cfg(&env);
        require_admin(&env, &c);
        c.deposits_enabled = deposits;
        c.swaps_enabled = swaps;
        c.paused = paused;
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn set_params(
        env: Env,
        reserve_factor_bps: u32,
        protocol_fee_bps: u32,
        max_pool_usd: i128,
        fee_receiver: Address,
    ) {
        let mut c = cfg(&env);
        require_admin(&env, &c);
        c.reserve_factor_bps = reserve_factor_bps;
        c.protocol_fee_bps = protocol_fee_bps;
        c.max_pool_usd = max_pool_usd;
        c.fee_receiver = fee_receiver;
        save_cfg(&env, &c);
        bump(&env);
    }

    pub fn deposit(
        env: Env,
        user: Address,
        long_amount: i128,
        short_amount: i128,
        min_shares: i128,
    ) -> i128 {
        user.require_auth();
        let c = cfg(&env);
        if c.paused {
            panic_with_error!(&env, Error::Paused);
        }
        if !c.deposits_enabled {
            panic_with_error!(&env, Error::DepositsDisabled);
        }
        if long_amount < 0 || short_amount < 0 || (long_amount == 0 && short_amount == 0) {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        let mut s = state(&env);
        let value_before = if s.total_shares == 0 {
            0
        } else {
            pool_usd(&env, &c, &s)
        };
        if long_amount > 0 {
            transfer_in(&env, &c.long_token, &user, long_amount);
        }
        if short_amount > 0 {
            transfer_in(&env, &c.short_token, &user, short_amount);
        }
        let mut usd = 0i128;
        if long_amount > 0 {
            usd = usd.saturating_add(quote_usd(&env, &c, &c.long_token, long_amount));
        }
        if short_amount > 0 {
            usd = usd.saturating_add(quote_usd(&env, &c, &c.short_token, short_amount));
        }
        // Single-sided deposits pay impact for unbalancing token inventory.
        if long_amount == 0 || short_amount == 0 {
            let long_usd = quote_usd(&env, &c, &c.long_token, token_bal(&env, &c.long_token));
            let short_usd = quote_usd(&env, &c, &c.short_token, token_bal(&env, &c.short_token));
            let add_long = if long_amount > 0 {
                quote_usd(&env, &c, &c.long_token, long_amount)
            } else {
                0
            };
            let add_short = if short_amount > 0 {
                quote_usd(&env, &c, &c.short_token, short_amount)
            } else {
                0
            };
            let impact = price_impact_usd(
                long_usd.saturating_sub(add_long),
                short_usd.saturating_sub(add_short),
                long_usd,
                short_usd,
                usd,
                c.impact_factor_bps,
                c.impact_cap_bps,
            )
            .unwrap_or(0);
            if impact > 0 {
                usd = usd.saturating_sub(impact);
            }
        }
        if usd <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        let (minted, locked) = shares_to_mint(usd, s.total_shares, value_before.max(1))
            .unwrap_or_else(|_| panic_with_error!(&env, Error::Math));
        if minted < min_shares {
            panic_with_error!(&env, Error::Slippage);
        }
        s.total_shares = s
            .total_shares
            .checked_add(minted + locked)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Math));
        if c.max_pool_usd > 0 {
            let value_after = pool_usd(&env, &c, &s);
            if value_after > c.max_pool_usd {
                panic_with_error!(&env, Error::CapExceeded);
            }
        }
        save_state(&env, &s);
        share_token::mint_shares(&env, &user, minted);
        if locked > 0 {
            share_token::mint_shares(&env, &env.current_contract_address(), locked);
        }
        Deposited {
            user,
            long_amount,
            short_amount,
            shares: minted,
        }
        .publish(&env);
        bump(&env);
        minted
    }

    pub fn withdraw(
        env: Env,
        user: Address,
        shares: i128,
        min_long: i128,
        min_short: i128,
    ) -> (i128, i128) {
        user.require_auth();
        let c = cfg(&env);
        if c.paused {
            panic_with_error!(&env, Error::Paused);
        }
        if shares <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        let mut s = state(&env);
        if shares > share_token::share_balance(&env, &user) {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        let long_bal = token_bal(&env, &c.long_token);
        let short_bal = token_bal(&env, &c.short_token);
        let (long_out, short_out) = amounts_for_shares(shares, s.total_shares, long_bal, short_bal)
            .unwrap_or_else(|_| panic_with_error!(&env, Error::Math));
        let avail_l = long_bal.saturating_sub(s.reserved_long);
        let avail_s = short_bal.saturating_sub(s.reserved_short);
        if long_out > avail_l || short_out > avail_s {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        if long_out < min_long || short_out < min_short {
            panic_with_error!(&env, Error::Slippage);
        }
        share_token::burn_shares(&env, &user, shares);
        s.total_shares -= shares;
        save_state(&env, &s);
        transfer_out(&env, &c.long_token, &user, long_out);
        transfer_out(&env, &c.short_token, &user, short_out);
        Withdrawn {
            user,
            shares,
            long_amount: long_out,
            short_amount: short_out,
        }
        .publish(&env);
        bump(&env);
        (long_out, short_out)
    }

    pub fn swap(
        env: Env,
        user: Address,
        token_in: Address,
        amount_in: i128,
        min_out: i128,
        atomic: bool,
    ) -> i128 {
        user.require_auth();
        let c = cfg(&env);
        if c.paused {
            panic_with_error!(&env, Error::Paused);
        }
        if !c.swaps_enabled {
            panic_with_error!(&env, Error::SwapsDisabled);
        }
        let token_out = if token_in == c.long_token {
            c.short_token.clone()
        } else if token_in == c.short_token {
            c.long_token.clone()
        } else {
            panic_with_error!(&env, Error::BadToken);
        };
        transfer_in(&env, &token_in, &user, amount_in);
        let in_usd = quote_usd(&env, &c, &token_in, amount_in);
        let long_usd = quote_usd(&env, &c, &c.long_token, token_bal(&env, &c.long_token));
        let short_usd = quote_usd(&env, &c, &c.short_token, token_bal(&env, &c.short_token));
        let (before_l, before_s) = if is_long_token(&c, &token_in) {
            (long_usd.saturating_sub(in_usd), short_usd)
        } else {
            (long_usd, short_usd.saturating_sub(in_usd))
        };
        let improves = if is_long_token(&c, &token_in) {
            before_l > before_s
        } else {
            before_s > before_l
        };
        let fee_bps = if atomic {
            c.atomic_swap_fee_bps
        } else if improves {
            c.swap_fee_balanced_bps
        } else {
            c.swap_fee_imbalanced_bps
        };
        let fee = bps_of_ceil(in_usd, fee_bps).unwrap_or(0);
        let (pool_fee, proto_fee) = split_protocol_fee(fee, c.protocol_fee_bps).unwrap_or((fee, 0));
        let mut out_usd = in_usd.saturating_sub(fee);
        let impact = price_impact_usd(
            before_l,
            before_s,
            long_usd,
            short_usd,
            in_usd,
            c.impact_factor_bps,
            c.impact_cap_bps,
        )
        .unwrap_or(0);
        if impact > 0 {
            out_usd = out_usd.saturating_sub(impact);
        } else {
            out_usd = out_usd.saturating_add(-impact);
        }
        let out_price = OracleContractClient::new(&env, &c.oracle).quote(&token_out);
        let amount_out = lumix_core::from_usd(out_usd, out_price.mid())
            .unwrap_or_else(|_| panic_with_error!(&env, Error::Math));
        if amount_out < min_out {
            panic_with_error!(&env, Error::Slippage);
        }
        let s = state(&env);
        let avail = if is_long_token(&c, &token_out) {
            token_bal(&env, &c.long_token).saturating_sub(s.reserved_long)
        } else {
            token_bal(&env, &c.short_token).saturating_sub(s.reserved_short)
        };
        if amount_out > avail {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        transfer_out(&env, &token_out, &user, amount_out);
        if proto_fee > 0 {
            let proto_tokens = lumix_core::from_usd(proto_fee, out_price.mid()).unwrap_or(0);
            transfer_out(&env, &token_out, &c.fee_receiver, proto_tokens);
        }
        let _ = pool_fee;
        Swapped {
            user,
            token_in,
            amount_in,
            amount_out,
            fee,
            atomic,
        }
        .publish(&env);
        bump(&env);
        amount_out
    }

    /// Tracks trader funds separately from LP liquidity. Only the market can
    /// change liabilities, before paying collateral out or releasing fees.
    pub fn adjust_collateral(env: Env, delta: i128) {
        let c = cfg(&env);
        require_market(&env, &c);
        let previous: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Collateral)
            .unwrap_or(0);
        let next = previous
            .checked_add(delta)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Math));
        let balance =
            token::TokenClient::new(&env, &c.short_token).balance(&env.current_contract_address());
        if next < 0 || next > balance {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        env.storage().instance().set(&DataKey::Collateral, &next);
        bump(&env);
    }

    pub fn collateral(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::Collateral)
            .unwrap_or(0)
    }

    pub fn reserve(env: Env, is_long: bool, amount: i128) {
        let c = cfg(&env);
        require_market(&env, &c);
        if amount <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        let mut s = state(&env);
        let (bal, reserved) = if is_long {
            (token_bal(&env, &c.long_token), s.reserved_long)
        } else {
            (token_bal(&env, &c.short_token), s.reserved_short)
        };
        if !can_reserve(bal, reserved, amount, c.reserve_factor_bps).unwrap_or(false) {
            panic_with_error!(&env, Error::InsufficientLiquidity);
        }
        if is_long {
            s.reserved_long += amount;
        } else {
            s.reserved_short += amount;
        }
        save_state(&env, &s);
        bump(&env);
    }

    pub fn release(env: Env, is_long: bool, amount: i128) {
        let c = cfg(&env);
        require_market(&env, &c);
        let mut s = state(&env);
        if is_long {
            s.reserved_long = s.reserved_long.saturating_sub(amount.max(0));
        } else {
            s.reserved_short = s.reserved_short.saturating_sub(amount.max(0));
        }
        save_state(&env, &s);
        bump(&env);
    }

    pub fn pay(env: Env, to: Address, token: Address, amount: i128) {
        let c = cfg(&env);
        require_market(&env, &c);
        if token != c.long_token && token != c.short_token {
            panic_with_error!(&env, Error::BadToken);
        }
        transfer_out(&env, &token, &to, amount);
        bump(&env);
    }

    pub fn set_pending_pnl(env: Env, pnl: i128) {
        let c = cfg(&env);
        require_market(&env, &c);
        let mut s = state(&env);
        s.pending_trader_pnl = pnl;
        save_state(&env, &s);
        bump(&env);
    }

    pub fn config(env: Env) -> PoolConfig {
        cfg(&env)
    }

    pub fn get_state(env: Env) -> PoolState {
        state(&env)
    }

    pub fn pool_value(env: Env) -> i128 {
        let c = cfg(&env);
        let s = state(&env);
        pool_usd(&env, &c, &s)
    }

    /// Pool valuation at the same observation as a pending trade.
    pub fn pool_value_at(env: Env, timestamp: u64) -> i128 {
        let c = cfg(&env);
        let s = state(&env);
        let oracle = OracleContractClient::new(&env, &c.oracle);
        let long_usd = lumix_core::to_usd(
            token_bal(&env, &c.long_token),
            oracle.quote_at(&c.long_token, &timestamp).mid(),
        )
        .unwrap_or_else(|_| panic_with_error!(&env, Error::Math));
        let short_usd = lumix_core::to_usd(
            token_bal(&env, &c.short_token),
            oracle.quote_at(&c.short_token, &timestamp).mid(),
        )
        .unwrap_or_else(|_| panic_with_error!(&env, Error::Math));
        let pnl = cap_pending_pnl(
            s.pending_trader_pnl,
            long_usd + short_usd,
            c.max_pnl_factor_bps,
        )
        .unwrap_or(s.pending_trader_pnl);
        pool_value_usd(long_usd, short_usd, pnl)
            .unwrap_or_else(|_| panic_with_error!(&env, Error::Math))
    }

    pub fn oracle(env: Env) -> Address {
        cfg(&env).oracle
    }

    pub fn share_price(env: Env) -> i128 {
        let s = state(&env);
        if s.total_shares == 0 {
            return SCALAR_7;
        }
        usd_for_shares(SCALAR_7, s.total_shares, Self::pool_value(env)).unwrap_or(SCALAR_7)
    }

    pub fn long_token(env: Env) -> Address {
        cfg(&env).long_token
    }

    pub fn short_token(env: Env) -> Address {
        cfg(&env).short_token
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        share_token::transfer_shares(&env, &from, &to, amount);
    }

    pub fn transfer_from(env: Env, spender: Address, from: Address, to: Address, amount: i128) {
        spender.require_auth();
        share_token::spend_allowance(&env, &from, &spender, amount);
        share_token::transfer_shares(&env, &from, &to, amount);
    }

    pub fn approve(
        env: Env,
        from: Address,
        spender: Address,
        amount: i128,
        expiration_ledger: u32,
    ) {
        from.require_auth();
        share_token::approve_shares(&env, &from, &spender, amount, expiration_ledger);
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        share_token::share_balance(&env, &id)
    }

    pub fn allowance(env: Env, from: Address, spender: Address) -> i128 {
        share_token::allowance(&env, &from, &spender)
    }

    pub fn decimals(_env: Env) -> u32 {
        7
    }

    pub fn name(env: Env) -> String {
        share_token::name(&env)
    }

    pub fn symbol(env: Env) -> String {
        share_token::symbol(&env)
    }

    pub fn total_supply(env: Env) -> i128 {
        share_token::total_supply(&env)
    }
}

