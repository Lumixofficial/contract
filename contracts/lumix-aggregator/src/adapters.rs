use crate::{Error, Hop, Protocol};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    symbol_short, token, vec, Address, Env, IntoVal, Map, Symbol, TryFromVal, Val, Vec, U256,
};

/// Read pool identity from its own ABI. A protocol tag selects an adapter; it is
/// not an attestation that the pool belongs to a canonical factory.
pub fn resolve_pool(env: &Env, hop: &Hop) -> Result<(Vec<Address>, Option<i64>), Error> {
    let mut phoenix_fee = None;
    let tokens: Vec<Address> = match hop.protocol {
        Protocol::Soroswap | Protocol::Sushi => {
            let (first, second) = if hop.protocol == Protocol::Soroswap {
                ("token_0", "token_1")
            } else {
                ("token0", "token1")
            };
            vec![
                env,
                env.invoke_contract(&hop.pool, &Symbol::new(env, first), Vec::new(env)),
                env.invoke_contract(&hop.pool, &Symbol::new(env, second), Vec::new(env)),
            ]
        }
        Protocol::Aquarius | Protocol::Comet => {
            env.invoke_contract(&hop.pool, &Symbol::new(env, "get_tokens"), Vec::new(env))
        }
        Protocol::Phoenix => {
            // Read only the fields needed from the deployed config map. XYK and
            // stable pools can include different additional fields.
            let cfg: Map<Symbol, Val> =
                env.invoke_contract(&hop.pool, &Symbol::new(env, "query_config"), Vec::new(env));
            let a = cfg
                .get(symbol_short!("token_a"))
                .ok_or(Error::InvalidPool)?;
            let b = cfg
                .get(symbol_short!("token_b"))
                .ok_or(Error::InvalidPool)?;
            let fee = cfg
                .get(Symbol::new(env, "total_fee_bps"))
                .ok_or(Error::InvalidPool)?;
            let fee = i64::try_from_val(env, &fee).map_err(|_| Error::InvalidPool)?;
            if !(0..=10000).contains(&fee) {
                return Err(Error::InvalidFee);
            }
            phoenix_fee = Some(fee);
            vec![
                env,
                Address::try_from_val(env, &a).map_err(|_| Error::InvalidPool)?,
                Address::try_from_val(env, &b).map_err(|_| Error::InvalidPool)?,
            ]
        }
    };
    if tokens.len() < 2
        || tokens.len() > 8
        || !tokens.contains(&hop.token_in)
        || !tokens.contains(&hop.token_out)
    {
        return Err(Error::InvalidPool);
    }
    let mut seen = Vec::new(env);
    for token in tokens.iter() {
        if token == hop.pool || token == env.current_contract_address() || seen.contains(&token) {
            return Err(Error::InvalidPool);
        }
        seen.push_back(token);
    }
    Ok((tokens, phoenix_fee))
}

fn ceil_div(n: &U256, d: &U256) -> U256 {
    let q = n.div(d);
    if n.rem_euclid(d) == U256::from_u32(n.env(), 0) {
        q
    } else {
        q.add(&U256::from_u32(n.env(), 1))
    }
}
fn amount(value: U256) -> Result<i128, Error> {
    value
        .to_u128()
        .and_then(|n| i128::try_from(n).ok())
        .ok_or(Error::Overflow)
}

/// Soroswap charges ceil(input * 3 / 1000), before applying the invariant.
/// Wide intermediates avoid overflowing valid i128 amounts during quoting.
pub(crate) fn soroswap_amount(
    env: &Env,
    reserve_in: i128,
    reserve_out: i128,
    value: i128,
    exact_out: bool,
) -> Result<i128, Error> {
    if reserve_in <= 0 || reserve_out <= 0 || value <= 0 {
        return Err(Error::InvalidAmount);
    }
    let rin = U256::from_u128(env, reserve_in as u128);
    let rout = U256::from_u128(env, reserve_out as u128);
    let n = U256::from_u128(env, value as u128);
    let thousand = U256::from_u32(env, 1000);
    let retained = U256::from_u32(env, 997);
    if exact_out {
        if value >= reserve_out {
            return Err(Error::MaximumExceeded);
        }
        let net = ceil_div(&rin.mul(&n), &rout.sub(&n));
        // Reject oversized net amounts before multiplying by the fee scale.
        let net = amount(net)?;
        amount(ceil_div(
            &U256::from_u128(env, net as u128).mul(&thousand),
            &retained,
        ))
    } else {
        let net = n.mul(&retained).div(&thousand);
        amount(net.mul(&rout).div(&rin.add(&net)))
    }
}

fn authorize(env: &Env, token: &Address, function: &str, args: Vec<Val>) {
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: token.clone(),
                fn_name: Symbol::new(env, function),
                args,
            },
            sub_invocations: Vec::new(env),
        }),
    ]);
}
fn transfer_auth(env: &Env, hop: &Hop, amount: i128) {
    authorize(
        env,
        &hop.token_in,
        "transfer",
        (env.current_contract_address(), hop.pool.clone(), amount).into_val(env),
    );
}

/// Return values are deliberately ignored. The caller measures real token deltas.
pub fn execute(
    env: &Env,
    hop: &Hop,
    tokens: &Vec<Address>,
    phoenix_fee: Option<i64>,
    amount: i128,
    exact_out: bool,
    deadline: u64,
) -> Result<(), Error> {
    let owner = env.current_contract_address();
    let i = tokens
        .first_index_of(hop.token_in.clone())
        .ok_or(Error::InvalidPool)?;
    let j = tokens
        .first_index_of(hop.token_out.clone())
        .ok_or(Error::InvalidPool)?;
    let floor = if exact_out { hop.output } else { hop.limit };
    match hop.protocol {
        Protocol::Soroswap => {
            let (r0, r1): (i128, i128) =
                env.invoke_contract(&hop.pool, &Symbol::new(env, "get_reserves"), Vec::new(env));
            let (rin, rout) = if i == 0 { (r0, r1) } else { (r1, r0) };
            let (spend, output) = if exact_out {
                (
                    soroswap_amount(env, rin, rout, hop.output, true)?,
                    hop.output,
                )
            } else {
                (amount, soroswap_amount(env, rin, rout, amount, false)?)
            };
            if spend > amount {
                return Err(Error::MaximumExceeded);
            }
            if output < floor {
                return Err(Error::MinimumNotMet);
            }
            token::Client::new(env, &hop.token_in).transfer(&owner, &hop.pool, &spend);
            let (out0, out1) = if i == 0 {
                (0i128, output)
            } else {
                (output, 0i128)
            };
            let _: () = env.invoke_contract(
                &hop.pool,
                &symbol_short!("swap"),
                (out0, out1, owner).into_val(env),
            );
        }
        Protocol::Aquarius => {
            let spend = if exact_out {
                let n: u128 = env.invoke_contract(
                    &hop.pool,
                    &Symbol::new(env, "estimate_swap_strict_receive"),
                    (i, j, hop.output as u128).into_val(env),
                );
                i128::try_from(n).map_err(|_| Error::Overflow)?
            } else {
                amount
            };
            if spend <= 0 || spend > amount {
                return Err(Error::MaximumExceeded);
            }
            // Aquarius pulls the cap first and refunds the excess internally.
            transfer_auth(env, hop, amount);
            let _: u128 = env.invoke_contract(
                &hop.pool,
                &Symbol::new(
                    env,
                    if exact_out {
                        "swap_strict_receive"
                    } else {
                        "swap"
                    },
                ),
                if exact_out {
                    (owner, i, j, hop.output as u128, amount as u128).into_val(env)
                } else {
                    (owner, i, j, amount as u128, floor as u128).into_val(env)
                },
            );
        }
        Protocol::Phoenix => {
            transfer_auth(env, hop, amount);
            let _: i128 = env.invoke_contract(
                &hop.pool,
                &symbol_short!("swap"),
                (
                    owner,
                    hop.token_in.clone(),
                    amount,
                    Some(floor),
                    hop.max_spread_bps.map(i64::from),
                    Some(deadline),
                    phoenix_fee,
                )
                    .into_val(env),
            );
        }
        Protocol::Sushi => {
            let hints: Val = env.invoke_contract(
                &hop.pool,
                &Symbol::new(env, "get_oracle_hints"),
                Vec::new(env),
            );
            let price = if i == 0 {
                U256::from_parts(env, 0, 0, 0, 4295128740)
            } else {
                U256::from_parts(
                    env,
                    0,
                    4294805859,
                    17280870778742802505,
                    6743328256752651557,
                )
            };
            // Direct invoker authorization covers pool.swap. Only its nested
            // input-token transfer needs an explicit authorization entry.
            transfer_auth(env, hop, amount);
            let _: Val = env.invoke_contract(
                &hop.pool,
                &symbol_short!("swap"),
                (owner.clone(), owner, i == 0, amount, price, hints).into_val(env),
            );
        }
        Protocol::Comet => {
            let expiration = (env.ledger().sequence() / 100000 + 1)
                .checked_mul(100000)
                .ok_or(Error::Overflow)?;
            authorize(
                env,
                &hop.token_in,
                "approve",
                (owner.clone(), hop.pool.clone(), amount, expiration).into_val(env),
            );
            let _: (i128, i128) = env.invoke_contract(
                &hop.pool,
                &Symbol::new(
                    env,
                    if exact_out {
                        "swap_exact_amount_out"
                    } else {
                        "swap_exact_amount_in"
                    },
                ),
                (
                    hop.token_in.clone(),
                    amount,
                    hop.token_out.clone(),
                    floor,
                    hop.max_price.unwrap_or(i128::MAX),
                    owner.clone(),
                )
                    .into_val(env),
            );
            // Comet approves the cap on exact out. Remove the unused allowance atomically.
            token::Client::new(env, &hop.token_in).approve(&owner, &hop.pool, &0, &expiration);
        }
    }
    Ok(())
}
