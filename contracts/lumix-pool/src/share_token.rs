use lumix_core::{INSTANCE_TTL_EXTEND_TO, INSTANCE_TTL_THRESHOLD};
use soroban_sdk::{contracttype, panic_with_error, symbol_short, Address, Env, String};

use crate::Error;

#[contracttype]
#[derive(Clone)]
pub enum TokenKey {
    Balance(Address),
    Allowance(Address, Address),
    Total,
}

fn bump(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

pub fn share_balance(env: &Env, id: &Address) -> i128 {
    env.storage()
        .persistent()
        .get(&TokenKey::Balance(id.clone()))
        .unwrap_or(0)
}

pub fn total_supply(env: &Env) -> i128 {
    env.storage().instance().get(&TokenKey::Total).unwrap_or(0)
}

fn set_balance(env: &Env, id: &Address, amount: i128) {
    let key = TokenKey::Balance(id.clone());
    env.storage().persistent().set(&key, &amount);
    env.storage().persistent().extend_ttl(
        &key,
        lumix_core::PERSISTENT_TTL_THRESHOLD,
        lumix_core::PERSISTENT_TTL_EXTEND_TO,
    );
}

pub fn mint_shares(env: &Env, to: &Address, amount: i128) {
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    let bal = share_balance(env, to);
    set_balance(env, to, bal + amount);
    env.storage()
        .instance()
        .set(&TokenKey::Total, &(total_supply(env) + amount));
    bump(env);
}

pub fn burn_shares(env: &Env, from: &Address, amount: i128) {
    if amount <= 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    let bal = share_balance(env, from);
    if bal < amount {
        panic_with_error!(env, Error::InsufficientLiquidity);
    }
    set_balance(env, from, bal - amount);
    env.storage()
        .instance()
        .set(&TokenKey::Total, &(total_supply(env) - amount));
    bump(env);
}

pub fn transfer_shares(env: &Env, from: &Address, to: &Address, amount: i128) {
    if amount < 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    if amount == 0 || from == to {
        return;
    }
    let from_bal = share_balance(env, from);
    if from_bal < amount {
        panic_with_error!(env, Error::InsufficientLiquidity);
    }
    set_balance(env, from, from_bal - amount);
    set_balance(env, to, share_balance(env, to) + amount);
    env.events().publish(
        (symbol_short!("transfer"), from.clone(), to.clone()),
        amount,
    );
    bump(env);
}

pub fn allowance(env: &Env, from: &Address, spender: &Address) -> i128 {
    env.storage()
        .temporary()
        .get(&TokenKey::Allowance(from.clone(), spender.clone()))
        .unwrap_or(0)
}

pub fn approve_shares(env: &Env, from: &Address, spender: &Address, amount: i128, _exp: u32) {
    if amount < 0 {
        panic_with_error!(env, Error::InvalidAmount);
    }
    env.storage()
        .temporary()
        .set(&TokenKey::Allowance(from.clone(), spender.clone()), &amount);
    bump(env);
}

pub fn spend_allowance(env: &Env, from: &Address, spender: &Address, amount: i128) {
    let a = allowance(env, from, spender);
    if a < amount {
        panic_with_error!(env, Error::Unauthorized);
    }
    approve_shares(env, from, spender, a - amount, 0);
}

pub fn name(env: &Env) -> String {
    env.storage()
        .instance()
        .get(&crate::DataKey::ShareName)
        .unwrap_or(String::from_str(env, "Lumix GM"))
}

pub fn symbol(env: &Env) -> String {
    env.storage()
        .instance()
        .get(&crate::DataKey::ShareSymbol)
        .unwrap_or(String::from_str(env, "GM"))
}
