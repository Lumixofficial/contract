#![no_std]

use lumix_core::{
    option_buyer_payout, option_writer_collateral, INSTANCE_TTL_THRESHOLD, PERSISTENT_TTL_THRESHOLD,
};
use lumix_interfaces::OracleContractClient;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, token,
    Address, Env,
};

/// Supported expiries may be up to 180 days away when enabled.
pub const MAX_EXPIRY_SECONDS: u64 = 180 * 86_400;
/// Keeps option orders and positions alive for about 231 days at five seconds per ledger.
pub const OPTION_TTL_EXTEND_TO: u32 = 4_000_000;

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, OPTION_TTL_EXTEND_TO);
}

fn bump_key(env: &Env, key: &DataKey) {
    env.storage()
        .persistent()
        .extend_ttl(key, PERSISTENT_TTL_THRESHOLD, OPTION_TTL_EXTEND_TO);
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    Paused = 2,
    InvalidAmount = 3,
    InvalidPair = 4,
    UnsupportedPair = 5,
    UnsupportedExpiry = 6,
    InvalidExpiry = 7,
    Missing = 8,
    BadStatus = 9,
    NotCrossed = 10,
    Mismatch = 11,
    SelfMatch = 12,
    NotExpired = 13,
    SettlementMissing = 14,
    SettlementExists = 15,
    Math = 16,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionKind {
    Call,
    Put,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderStatus {
    Open,
    Matched,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PositionStatus {
    Open,
    Settled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub admin: Address,
    pub oracle: Address,
    pub paused: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderRequest {
    /// Base units in Stellar's seven-decimal token scale.
    pub amount: i128,
    pub base: Address,
    pub expiry: u64,
    pub kind: OptionKind,
    /// Total premium for the whole order, in quote-token units.
    pub premium: i128,
    pub quote: Address,
    /// Strike as quote per base, using Lumix's 14-decimal price scale.
    pub strike: i128,
    pub side: OrderSide,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub amount: i128,
    pub base: Address,
    pub created_at: u64,
    pub escrowed: i128,
    pub expiry: u64,
    pub id: u32,
    pub kind: OptionKind,
    pub position_id: Option<u32>,
    pub premium: i128,
    pub quote: Address,
    pub side: OrderSide,
    pub status: OrderStatus,
    pub strike: i128,
    pub trader: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Position {
    pub opened_at: u64,
    pub validated_at: u64,
    pub buyer_price: i128,
    pub seller_price: i128,
    pub amount: i128,
    pub base: Address,
    pub buyer: Address,
    pub buyer_payout: i128,
    pub collateral: i128,
    pub expiry: u64,
    pub id: u32,
    pub kind: OptionKind,
    pub premium: i128,
    pub quote: Address,
    pub seller: Address,
    pub seller_payout: i128,
    pub settlement_price: i128,
    pub status: PositionStatus,
    pub strike: i128,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    Keeper(Address),
    NextOrder,
    NextPosition,
    Pair(Address, Address),
    Expiry(Address, Address, u64),
    Order(u32),
    Position(u32),
}

#[contractevent]
pub struct PairUpdated {
    pub base: Address,
    pub enabled: bool,
    pub quote: Address,
}

#[contractevent]
pub struct ExpiryUpdated {
    pub base: Address,
    pub enabled: bool,
    pub expiry: u64,
    pub quote: Address,
}

#[contractevent]
pub struct OrderCreated {
    pub timestamp: u64,
    pub expiry: u64,
    pub id: u32,
    pub trader: Address,
}

#[contractevent]
pub struct OrderCancelled {
    pub id: u32,
    pub trader: Address,
}

#[contractevent]
pub struct OrdersMatched {
    pub buy_order: u32,
    pub position_id: u32,
    pub premium: i128,
    pub sell_order: u32,
}

#[contractevent]
pub struct PositionSettled {
    pub buyer_payout: i128,
    pub id: u32,
    pub seller_payout: i128,
    pub settlement_price: i128,
}

#[contract]
pub struct Options;

fn config(env: &Env) -> Config {
    env.storage()
        .instance()
        .get(&DataKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, Error::Unauthorized))
}

fn require_admin(c: &Config) {
    c.admin.require_auth();
}

fn require_settler(env: &Env, caller: &Address, c: &Config) {
    caller.require_auth();
    if *caller == c.admin {
        return;
    }
    let allowed: bool = env
        .storage()
        .instance()
        .get(&DataKey::Keeper(caller.clone()))
        .unwrap_or(false);
    if !allowed {
        panic_with_error!(env, Error::Unauthorized);
    }
}

fn pair_key(base: &Address, quote: &Address) -> DataKey {
    DataKey::Pair(base.clone(), quote.clone())
}

fn expiry_key(base: &Address, quote: &Address, expiry: u64) -> DataKey {
    DataKey::Expiry(base.clone(), quote.clone(), expiry)
}

fn pair_supported(env: &Env, base: &Address, quote: &Address) -> bool {
    env.storage()
        .persistent()
        .get(&pair_key(base, quote))
        .unwrap_or(false)
}

fn expiry_supported(env: &Env, base: &Address, quote: &Address, expiry: u64) -> bool {
    env.storage()
        .persistent()
        .get(&expiry_key(base, quote, expiry))
        .unwrap_or(false)
}

fn load_order(env: &Env, id: u32) -> Order {
    let key = DataKey::Order(id);
    let order = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, Error::Missing));
    bump_key(env, &key);
    order
}

fn save_order(env: &Env, order: &Order) {
    let key = DataKey::Order(order.id);
    env.storage().persistent().set(&key, order);
    bump_key(env, &key);
}

fn load_position(env: &Env, id: u32) -> Position {
    let key = DataKey::Position(id);
    let position = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, Error::Missing));
    bump_key(env, &key);
    position
}

fn save_position(env: &Env, position: &Position) {
    let key = DataKey::Position(position.id);
    env.storage().persistent().set(&key, position);
    bump_key(env, &key);
}

fn next_id(env: &Env, key: DataKey) -> u32 {
    let current: u32 = env.storage().instance().get(&key).unwrap_or(0);
    let next = current
        .checked_add(1)
        .unwrap_or_else(|| panic_with_error!(env, Error::Math));
    env.storage().instance().set(&key, &next);
    next
}

fn is_call(kind: &OptionKind) -> bool {
    *kind == OptionKind::Call
}

fn writer_collateral(env: &Env, request: &OrderRequest) -> i128 {
    option_writer_collateral(request.amount, request.strike, is_call(&request.kind))
        .unwrap_or_else(|_| panic_with_error!(env, Error::Math))
}

fn transfer(env: &Env, token_address: &Address, from: &Address, to: &Address, amount: i128) {
    if amount > 0 {
        token::TokenClient::new(env, token_address).transfer(from, to, &amount);
    }
}

fn same_series(a: &Order, b: &Order) -> bool {
    a.base == b.base
        && a.quote == b.quote
        && a.expiry == b.expiry
        && a.strike == b.strike
        && a.kind == b.kind
        && a.amount == b.amount
}

fn pair_price(env: &Env, c: &Config, base: &Address, quote: &Address, timestamp: u64) -> i128 {
    let oracle = OracleContractClient::new(env, &c.oracle);
    let base_price = oracle.quote_at(base, &timestamp).mid();
    let quote_price = oracle.quote_at(quote, &timestamp).mid();
    let price = lumix_core::mul_div_floor(base_price, lumix_core::PRICE_SCALE, quote_price)
        .unwrap_or_else(|_| panic_with_error!(env, Error::Math));
    if price <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    price
}

#[contractimpl]
impl Options {
    pub fn __constructor(env: Env, admin: Address, oracle: Address) {
        env.storage().instance().set(
            &DataKey::Config,
            &Config {
                admin,
                oracle,
                paused: false,
            },
        );
        env.storage().instance().set(&DataKey::NextOrder, &0u32);
        env.storage().instance().set(&DataKey::NextPosition, &0u32);
        bump(&env);
    }

    pub fn set_paused(env: Env, paused: bool) {
        let mut c = config(&env);
        require_admin(&c);
        c.paused = paused;
        env.storage().instance().set(&DataKey::Config, &c);
        bump(&env);
    }

    pub fn set_keeper(env: Env, keeper: Address, allowed: bool) {
        let c = config(&env);
        require_admin(&c);
        env.storage()
            .instance()
            .set(&DataKey::Keeper(keeper), &allowed);
        bump(&env);
    }

    pub fn set_pair(env: Env, base: Address, quote: Address, enabled: bool) {
        let c = config(&env);
        require_admin(&c);
        if base == quote {
            panic_with_error!(&env, Error::InvalidPair);
        }
        let key = pair_key(&base, &quote);
        env.storage().persistent().set(&key, &enabled);
        bump_key(&env, &key);
        PairUpdated {
            base,
            enabled,
            quote,
        }
        .publish(&env);
        bump(&env);
    }

    pub fn set_expiry(env: Env, base: Address, quote: Address, expiry: u64, enabled: bool) {
        let c = config(&env);
        require_admin(&c);
        let key = expiry_key(&base, &quote, expiry);
        if enabled {
            let now = env.ledger().timestamp();
            if !pair_supported(&env, &base, &quote) {
                panic_with_error!(&env, Error::UnsupportedPair);
            }
            if expiry <= now || expiry.saturating_sub(now) > MAX_EXPIRY_SECONDS {
                panic_with_error!(&env, Error::InvalidExpiry);
            }
        } else if !env.storage().persistent().has(&key) {
            panic_with_error!(&env, Error::UnsupportedExpiry);
        }
        env.storage().persistent().set(&key, &enabled);
        bump_key(&env, &key);
        ExpiryUpdated {
            base,
            enabled,
            expiry,
            quote,
        }
        .publish(&env);
        bump(&env);
    }

    pub fn create_order(env: Env, trader: Address, request: OrderRequest) -> u32 {
        trader.require_auth();
        let c = config(&env);
        if c.paused {
            panic_with_error!(&env, Error::Paused);
        }
        if request.base == request.quote {
            panic_with_error!(&env, Error::InvalidPair);
        }
        if request.amount <= 0 || request.strike <= 0 || request.premium <= 0 {
            panic_with_error!(&env, Error::InvalidAmount);
        }
        if request.expiry <= env.ledger().timestamp() {
            panic_with_error!(&env, Error::InvalidExpiry);
        }
        if !pair_supported(&env, &request.base, &request.quote) {
            panic_with_error!(&env, Error::UnsupportedPair);
        }
        if !expiry_supported(&env, &request.base, &request.quote, request.expiry) {
            panic_with_error!(&env, Error::UnsupportedExpiry);
        }

        let escrowed = if request.side == OrderSide::Buy {
            request.premium
        } else {
            writer_collateral(&env, &request)
        };
        let escrow_token = if request.side == OrderSide::Buy || request.kind == OptionKind::Put {
            request.quote.clone()
        } else {
            request.base.clone()
        };
        transfer(
            &env,
            &escrow_token,
            &trader,
            &env.current_contract_address(),
            escrowed,
        );

        let id = next_id(&env, DataKey::NextOrder);
        let order = Order {
            amount: request.amount,
            base: request.base,
            created_at: env.ledger().timestamp(),
            escrowed,
            expiry: request.expiry,
            id,
            kind: request.kind,
            position_id: None,
            premium: request.premium,
            quote: request.quote,
            side: request.side,
            status: OrderStatus::Open,
            strike: request.strike,
            trader: trader.clone(),
        };
        save_order(&env, &order);
        OrderCreated {
            timestamp: order.created_at,
            expiry: order.expiry,
            id,
            trader,
        }
        .publish(&env);
        bump(&env);
        id
    }

    pub fn cancel_order(env: Env, trader: Address, id: u32) {
        trader.require_auth();
        let mut order = load_order(&env, id);
        if order.trader != trader {
            panic_with_error!(&env, Error::Unauthorized);
        }
        if order.status != OrderStatus::Open {
            panic_with_error!(&env, Error::BadStatus);
        }
        order.status = OrderStatus::Cancelled;
        save_order(&env, &order);
        let escrow_token = if order.side == OrderSide::Buy || order.kind == OptionKind::Put {
            order.quote.clone()
        } else {
            order.base.clone()
        };
        transfer(
            &env,
            &escrow_token,
            &env.current_contract_address(),
            &trader,
            order.escrowed,
        );
        OrderCancelled { id, trader }.publish(&env);
        bump(&env);
    }

    /// Keeper-validated exact-size match. The older order is the maker and sets
    /// the execution premium; the buyer's unused bid escrow is refunded.
    pub fn match_orders(env: Env, caller: Address, first_id: u32, second_id: u32) -> u32 {
        let c = config(&env);
        require_settler(&env, &caller, &c);
        if c.paused {
            panic_with_error!(&env, Error::Paused);
        }
        if first_id == second_id {
            panic_with_error!(&env, Error::Mismatch);
        }
        let first = load_order(&env, first_id);
        let second = load_order(&env, second_id);
        if first.status != OrderStatus::Open || second.status != OrderStatus::Open {
            panic_with_error!(&env, Error::BadStatus);
        }
        let (mut buy, mut sell) = if first.side == OrderSide::Buy && second.side == OrderSide::Sell
        {
            (first, second)
        } else if first.side == OrderSide::Sell && second.side == OrderSide::Buy {
            (second, first)
        } else {
            panic_with_error!(&env, Error::Mismatch);
        };
        if buy.trader == sell.trader {
            panic_with_error!(&env, Error::SelfMatch);
        }
        if !same_series(&buy, &sell) {
            panic_with_error!(&env, Error::Mismatch);
        }
        if env.ledger().timestamp() >= buy.expiry {
            panic_with_error!(&env, Error::InvalidExpiry);
        }
        if !pair_supported(&env, &buy.base, &buy.quote)
            || !expiry_supported(&env, &buy.base, &buy.quote, buy.expiry)
        {
            panic_with_error!(&env, Error::UnsupportedExpiry);
        }
        if buy.premium < sell.premium {
            panic_with_error!(&env, Error::NotCrossed);
        }

        // Each request must have its own historical observation. Premium and
        // strike remain the prices agreed by the buyer and writer.
        let buyer_price = pair_price(&env, &c, &buy.base, &buy.quote, buy.created_at);
        let seller_price = pair_price(&env, &c, &sell.base, &sell.quote, sell.created_at);
        let execution_premium = if buy.id < sell.id {
            buy.premium
        } else {
            sell.premium
        };
        let refund = buy
            .escrowed
            .checked_sub(execution_premium)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Math));
        let position_id = next_id(&env, DataKey::NextPosition);
        buy.status = OrderStatus::Matched;
        buy.position_id = Some(position_id);
        sell.status = OrderStatus::Matched;
        sell.position_id = Some(position_id);
        save_order(&env, &buy);
        save_order(&env, &sell);
        save_position(
            &env,
            &Position {
                opened_at: buy.created_at.max(sell.created_at),
                validated_at: env.ledger().timestamp(),
                buyer_price,
                seller_price,
                amount: buy.amount,
                base: buy.base.clone(),
                buyer: buy.trader.clone(),
                buyer_payout: 0,
                collateral: sell.escrowed,
                expiry: buy.expiry,
                id: position_id,
                kind: buy.kind.clone(),
                premium: execution_premium,
                quote: buy.quote.clone(),
                seller: sell.trader.clone(),
                seller_payout: 0,
                settlement_price: 0,
                status: PositionStatus::Open,
                strike: buy.strike,
            },
        );
        transfer(
            &env,
            &buy.quote,
            &env.current_contract_address(),
            &buy.trader,
            refund,
        );
        OrdersMatched {
            buy_order: buy.id,
            position_id,
            premium: execution_premium,
            sell_order: sell.id,
        }
        .publish(&env);
        bump(&env);
        position_id
    }

    /// European settlement at the exact expiry observation. A keeper submits
    /// the transaction after expiry; anyone can retry it once prices exist.
    pub fn settle(env: Env, position_id: u32) -> Position {
        let mut position = load_position(&env, position_id);
        if position.status != PositionStatus::Open {
            panic_with_error!(&env, Error::BadStatus);
        }
        if env.ledger().timestamp() < position.expiry {
            panic_with_error!(&env, Error::NotExpired);
        }
        let settlement_price = pair_price(
            &env,
            &config(&env),
            &position.base,
            &position.quote,
            position.expiry,
        );
        let buyer_payout = option_buyer_payout(
            position.amount,
            position.strike,
            settlement_price,
            is_call(&position.kind),
        )
        .unwrap_or_else(|_| panic_with_error!(&env, Error::Math));
        let seller_payout = position
            .collateral
            .checked_sub(buyer_payout)
            .unwrap_or_else(|| panic_with_error!(&env, Error::Math));
        position.buyer_payout = buyer_payout;
        position.seller_payout = seller_payout;
        position.settlement_price = settlement_price;
        position.status = PositionStatus::Settled;
        save_position(&env, &position);

        let contract = env.current_contract_address();
        if position.kind == OptionKind::Call {
            transfer(
                &env,
                &position.base,
                &contract,
                &position.buyer,
                buyer_payout,
            );
            transfer(
                &env,
                &position.base,
                &contract,
                &position.seller,
                seller_payout,
            );
            transfer(
                &env,
                &position.quote,
                &contract,
                &position.seller,
                position.premium,
            );
        } else {
            transfer(
                &env,
                &position.quote,
                &contract,
                &position.buyer,
                buyer_payout,
            );
            let seller_quote = seller_payout
                .checked_add(position.premium)
                .unwrap_or_else(|| panic_with_error!(&env, Error::Math));
            transfer(
                &env,
                &position.quote,
                &contract,
                &position.seller,
                seller_quote,
            );
        }
        PositionSettled {
            buyer_payout,
            id: position_id,
            seller_payout,
            settlement_price,
        }
        .publish(&env);
        bump(&env);
        position
    }

    pub fn config(env: Env) -> Config {
        config(&env)
    }

    pub fn is_pair_supported(env: Env, base: Address, quote: Address) -> bool {
        pair_supported(&env, &base, &quote)
    }

    pub fn is_expiry_supported(env: Env, base: Address, quote: Address, expiry: u64) -> bool {
        expiry_supported(&env, &base, &quote, expiry)
    }

    pub fn get_order(env: Env, id: u32) -> Order {
        load_order(&env, id)
    }

    pub fn get_position(env: Env, id: u32) -> Position {
        load_position(&env, id)
    }

    pub fn get_settlement(env: Env, base: Address, quote: Address, expiry: u64) -> i128 {
        if env.ledger().timestamp() < expiry {
            panic_with_error!(&env, Error::NotExpired);
        }
        pair_price(&env, &config(&env), &base, &quote, expiry)
    }
}

