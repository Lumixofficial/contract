use soroban_sdk::{contracterror, contracttype, Address, Vec};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Protocol {
    Soroswap,
    Aquarius,
    Phoenix,
    Sushi,
    Comet,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub admin: Address,
    pub treasury: Address,
    pub fee_bps: u32,
}

/// Exact in: output = 0, limit = minimum output.
/// Exact out: output = target output, limit = maximum input.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hop {
    pub protocol: Protocol,
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub output: i128,
    pub limit: i128,
    pub max_spread_bps: Option<u32>,
    pub max_price: Option<i128>,
}

/// Exact in: amount = input allocation, limit = gross output floor.
/// Exact out: amount = gross output allocation, limit = root input cap.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Leg {
    pub amount: i128,
    pub limit: i128,
    pub hops: Vec<Hop>,
}

/// Exact in: amount = total input, limit = minimum NET received.
/// Exact out: amount = exact NET delivered, limit = total maximum input.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapRequest {
    pub sender: Address,
    pub recipient: Option<Address>,
    pub token_in: Address,
    pub token_out: Address,
    pub amount: i128,
    pub limit: i128,
    pub deadline: u64,
    pub fee_bps: u32,
    pub legs: Vec<Leg>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapResult {
    pub amount_in: i128,
    pub amount_out: i128,
    pub fee: i128,
    pub surplus_out: i128,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    InvalidAmount = 1,
    InvalidFee = 2,
    FeeChanged = 3,
    Expired = 4,
    InvalidRoute = 5,
    TooManySteps = 6,
    InvalidPool = 8,
    InconsistentTotals = 9,
    MinimumNotMet = 10,
    MaximumExceeded = 11,
    BalanceInvariant = 12,
    Overflow = 13,
    InvalidRecipient = 14,
}
