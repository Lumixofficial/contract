#![no_std]
mod adapters;
mod types;
use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype, panic_with_error, token, Address, Env,
    Map, Vec,
};
pub use types::*;

const BPS: i128 = 10000;
pub const MAX_LEGS: u32 = 8;
pub const MAX_HOPS: u32 = 4;
const TTL_MIN: u32 = 17280;
const TTL_MAX: u32 = 518400;

#[contracttype]
#[derive(Clone)]
enum Key {
    Config,
}

#[contractevent]
pub struct Swapped {
    pub sender: Address,
    pub recipient: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub exact_out: bool,
    pub result: SwapResult,
}
#[contractevent]
pub struct FeeChanged {
    pub fee_bps: u32,
    pub treasury: Address,
}
#[contract]
pub struct Aggregator;

fn add(a: i128, b: i128) -> Result<i128, Error> {
    a.checked_add(b).ok_or(Error::Overflow)
}
pub fn fee(amount: i128, bps: u32) -> Result<i128, Error> {
    if amount < 0 {
        return Err(Error::InvalidAmount);
    }
    if bps >= 10000 {
        return Err(Error::InvalidFee);
    }
    Ok(amount / BPS * i128::from(bps) + amount % BPS * i128::from(bps) / BPS)
}
pub fn gross_for_net(net: i128, bps: u32) -> Result<i128, Error> {
    if net <= 0 {
        return Err(Error::InvalidAmount);
    }
    if bps >= 10000 {
        return Err(Error::InvalidFee);
    }
    // Minimal gross whose gross - floor(gross * bps / 10000) equals net.
    let n = net - 1;
    let d = BPS - i128::from(bps);
    let f = (n / d)
        .checked_mul(i128::from(bps))
        .ok_or(Error::Overflow)?;
    add(net, add(f, n % d * i128::from(bps) / d)?)
}
fn config(env: &Env) -> Config {
    env.storage().instance().extend_ttl(TTL_MIN, TTL_MAX);
    env.storage().instance().get(&Key::Config).unwrap()
}
fn balance(env: &Env, asset: &Address) -> i128 {
    token::Client::new(env, asset).balance(&env.current_contract_address())
}

type PoolDetails = Map<Address, (Vec<Address>, Option<i64>)>;

fn validate(
    env: &Env,
    req: &SwapRequest,
    cfg: &Config,
    exact_out: bool,
) -> Result<(i128, Map<Address, i128>, PoolDetails), Error> {
    if req.amount <= 0 || req.limit <= 0 {
        return Err(Error::InvalidAmount);
    }
    if req.fee_bps != cfg.fee_bps {
        return Err(Error::FeeChanged);
    }
    if req.deadline < env.ledger().timestamp() {
        return Err(Error::Expired);
    }
    let owner = env.current_contract_address();
    if req.sender == owner || req.recipient == Some(owner.clone()) {
        return Err(Error::InvalidRecipient);
    }
    if req.token_in == req.token_out || req.legs.is_empty() {
        return Err(Error::InvalidRoute);
    }
    if req.legs.len() > MAX_LEGS {
        return Err(Error::TooManySteps);
    }
    let mut seen_pools = Vec::new(env);
    let mut assets = Map::new(env);
    let mut pools = Map::new(env);
    let mut amounts = 0;
    let mut limits = 0;
    for leg in req.legs.iter() {
        if leg.amount <= 0 || leg.limit <= 0 {
            return Err(Error::InvalidAmount);
        }
        if leg.hops.is_empty() {
            return Err(Error::InvalidRoute);
        }
        if leg.hops.len() > MAX_HOPS {
            return Err(Error::TooManySteps);
        }
        let mut current = req.token_in.clone();
        let mut path = Vec::new(env);
        path.push_back(current.clone());
        for (idx, hop) in leg.hops.iter().enumerate() {
            if hop.token_in != current || path.contains(&hop.token_out) {
                return Err(Error::InvalidRoute);
            }
            if seen_pools.contains(&hop.pool) {
                return Err(Error::InvalidRoute);
            }
            if hop.pool == owner || hop.pool == hop.token_in || hop.pool == hop.token_out {
                return Err(Error::InvalidPool);
            }
            seen_pools.push_back(hop.pool.clone());
            path.push_back(hop.token_out.clone());
            let details = adapters::resolve_pool(env, &hop)?;
            pools.set(hop.pool.clone(), details);
            if hop.limit <= 0 || (exact_out && hop.output <= 0) || (!exact_out && hop.output != 0) {
                return Err(Error::InvalidAmount);
            }
            if hop.max_spread_bps.is_some_and(|n| n > 10000)
                || hop.max_price.is_some_and(|n| n <= 0)
            {
                return Err(Error::InvalidAmount);
            }
            if exact_out {
                if idx == 0 && hop.limit != leg.limit {
                    return Err(Error::InconsistentTotals);
                }
                if let Some(next) = leg.hops.get(idx as u32 + 1) {
                    if hop.output != next.limit {
                        return Err(Error::InconsistentTotals);
                    }
                } else if hop.output != leg.amount {
                    return Err(Error::InconsistentTotals);
                }
            } else if idx as u32 + 1 == leg.hops.len() && hop.limit != leg.limit {
                return Err(Error::InconsistentTotals);
            }
            assets.set(hop.token_in.clone(), 0);
            assets.set(hop.token_out.clone(), 0);
            current = hop.token_out;
        }
        if current != req.token_out {
            return Err(Error::InvalidRoute);
        }
        amounts = add(amounts, leg.amount)?;
        limits = add(limits, leg.limit)?;
    }
    let funding = if exact_out {
        if amounts != gross_for_net(req.amount, cfg.fee_bps)? || limits > req.limit {
            return Err(Error::InconsistentTotals);
        }
        limits
    } else {
        if amounts != req.amount || limits - fee(limits, cfg.fee_bps)? < req.limit {
            return Err(Error::InconsistentTotals);
        }
        amounts
    };
    for asset in assets.keys().iter() {
        assets.set(asset.clone(), balance(env, &asset));
    }
    Ok((funding, assets, pools))
}

fn swap(env: &Env, req: &SwapRequest, exact_out: bool) -> Result<SwapResult, Error> {
    req.sender.require_auth(); // Binds destination, all routes, limits, deadline, and fee.
    let cfg = config(env);
    let (funding, baselines, pools) = validate(env, req, &cfg, exact_out)?;
    let owner = env.current_contract_address();
    token::Client::new(env, &req.token_in).transfer(&req.sender, &owner, &funding);
    if balance(env, &req.token_in).checked_sub(baselines.get(req.token_in.clone()).unwrap())
        != Some(funding)
    {
        return Err(Error::BalanceInvariant);
    }
    for leg in req.legs.iter() {
        let mut available = if exact_out { leg.limit } else { leg.amount };
        for hop in leg.hops.iter() {
            let budget = if exact_out { hop.limit } else { available };
            if budget > available {
                return Err(Error::MaximumExceeded);
            }
            let before_in = balance(env, &hop.token_in);
            let before_out = balance(env, &hop.token_out);
            if before_in
                .checked_sub(baselines.get(hop.token_in.clone()).unwrap())
                .ok_or(Error::Overflow)?
                < budget
            {
                return Err(Error::BalanceInvariant);
            }
            let (tokens, phoenix_fee) = pools.get(hop.pool.clone()).unwrap();
            adapters::execute(
                env,
                &hop,
                &tokens,
                phoenix_fee,
                budget,
                exact_out,
                req.deadline,
            )?;
            let spent = before_in
                .checked_sub(balance(env, &hop.token_in))
                .ok_or(Error::Overflow)?;
            let received = balance(env, &hop.token_out)
                .checked_sub(before_out)
                .ok_or(Error::Overflow)?;
            if spent <= 0 || spent > budget || (!exact_out && spent != budget) {
                return Err(Error::BalanceInvariant);
            }
            if received < if exact_out { hop.output } else { hop.limit } {
                return Err(Error::MinimumNotMet);
            }
            available = received;
        }
        if available < if exact_out { leg.amount } else { leg.limit } {
            return Err(Error::MinimumNotMet);
        }
    }
    let gross = balance(env, &req.token_out)
        .checked_sub(baselines.get(req.token_out.clone()).unwrap())
        .ok_or(Error::Overflow)?;
    let (delivered, charged, surplus) = if exact_out {
        let target = gross_for_net(req.amount, cfg.fee_bps)?;
        if gross < target {
            return Err(Error::MinimumNotMet);
        }
        (req.amount, target - req.amount, gross - target)
    } else {
        let charged = fee(gross, cfg.fee_bps)?;
        if gross - charged < req.limit {
            return Err(Error::MinimumNotMet);
        }
        (gross - charged, charged, 0)
    };
    let recipient = req.recipient.clone().unwrap_or(req.sender.clone());
    let output = token::Client::new(env, &req.token_out);
    output.transfer(&owner, &recipient, &delivered);
    if charged > 0 {
        output.transfer(&owner, &cfg.treasury, &charged);
    }
    if surplus > 0 {
        output.transfer(&owner, &req.sender, &surplus);
    }
    let mut input_refund = 0;
    for (asset, baseline) in baselines.iter() {
        let refund = balance(env, &asset)
            .checked_sub(baseline)
            .ok_or(Error::Overflow)?;
        if refund < 0 {
            return Err(Error::BalanceInvariant);
        }
        if asset == req.token_in {
            input_refund = refund;
        }
        if refund > 0 {
            token::Client::new(env, &asset).transfer(&owner, &req.sender, &refund);
        }
        if balance(env, &asset) != baseline {
            return Err(Error::BalanceInvariant);
        }
    }
    let result = SwapResult {
        amount_in: funding - input_refund,
        amount_out: delivered,
        fee: charged,
        surplus_out: surplus,
    };
    Swapped {
        sender: req.sender.clone(),
        recipient,
        token_in: req.token_in.clone(),
        token_out: req.token_out.clone(),
        exact_out,
        result: result.clone(),
    }
    .publish(env);
    Ok(result)
}

#[contractimpl]
impl Aggregator {
    pub fn __constructor(env: Env, admin: Address, treasury: Address, fee_bps: Option<u32>) {
        let bps = fee_bps.unwrap_or(0);
        if bps >= 10000 {
            panic_with_error!(&env, Error::InvalidFee);
        }
        if treasury == env.current_contract_address() {
            panic_with_error!(&env, Error::InvalidRecipient);
        }
        env.storage().instance().set(
            &Key::Config,
            &Config {
                admin,
                treasury,
                fee_bps: bps,
            },
        );
        env.storage().instance().extend_ttl(TTL_MIN, TTL_MAX);
    }
    pub fn get_config(env: Env) -> Config {
        config(&env)
    }
    pub fn set_fee(env: Env, fee_bps: u32, treasury: Address) -> Result<(), Error> {
        let mut cfg = config(&env);
        cfg.admin.require_auth();
        if fee_bps >= 10000 {
            return Err(Error::InvalidFee);
        }
        if treasury == env.current_contract_address() {
            return Err(Error::InvalidRecipient);
        }
        cfg.fee_bps = fee_bps;
        cfg.treasury = treasury.clone();
        env.storage().instance().set(&Key::Config, &cfg);
        FeeChanged { fee_bps, treasury }.publish(&env);
        Ok(())
    }
    pub fn swap_exact_in(env: Env, request: SwapRequest) -> Result<SwapResult, Error> {
        swap(&env, &request, false)
    }
    pub fn swap_exact_out(env: Env, request: SwapRequest) -> Result<SwapResult, Error> {
        swap(&env, &request, true)
    }
}

