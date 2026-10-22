#[cfg(not(feature = "wasm-tests"))]
use lumix_aggregator::Aggregator;
use lumix_aggregator::{AggregatorClient, Hop, Leg, Protocol, SwapRequest};
use soroban_sdk::xdr::{
    LedgerEntry, LedgerEntryData, LedgerEntryExt, LedgerKey, Limits, ReadXdr, WriteXdr,
};
use soroban_sdk::{
    contracttype, symbol_short,
    testutils::{
        Address as _, EnvTestConfig, HostError, Ledger, MockAuth, MockAuthInvoke, SnapshotSource,
        SnapshotSourceInput,
    },
    token, vec, Address, Bytes, Env, IntoVal,
};
use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    rc::Rc,
};

struct RpcSource {
    process: RefCell<Child>,
    output: RefCell<BufReader<std::process::ChildStdout>>,
}
static FORK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
impl RpcSource {
    fn new() -> (Rc<Self>, serde_json::Value) {
        let mut child = Command::new("node")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fork-rpc.mjs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let source = Rc::new(Self {
            process: RefCell::new(child),
            output: RefCell::new(BufReader::new(stdout)),
        });
        let info = source.query(serde_json::json!({"method":"info"}));
        (source, info)
    }
    fn query(&self, value: serde_json::Value) -> serde_json::Value {
        let mut child = self.process.borrow_mut();
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        self.output.borrow_mut().read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "Fork RPC process exited");
        let result: serde_json::Value = serde_json::from_str(&line).unwrap();
        result
    }
}
impl Drop for RpcSource {
    fn drop(&mut self) {
        let _ = self.process.get_mut().kill();
        let _ = self.process.get_mut().wait();
    }
}
impl SnapshotSource for RpcSource {
    fn get(
        &self,
        key: &Rc<LedgerKey>,
    ) -> Result<Option<(Rc<LedgerEntry>, Option<u32>)>, HostError> {
        // These are new LOCAL fixtures, not Mainnet entries. Upload probes
        // the code key before writing it; the local Wasm naturally is absent.
        #[cfg(feature = "wasm-tests")]
        if let LedgerKey::ContractCode(code) = key.as_ref() {
            use sha2::Digest;
            let hash: [u8; 32] = sha2::Sha256::digest(include_bytes!(
                "../target/wasm32v1-none/release/lumix_aggregator.wasm"
            ))
            .into();
            if code.hash.0 == hash {
                return Ok(None);
            }
        }
        if let LedgerKey::ContractData(data) = key.as_ref() {
            if let soroban_sdk::xdr::ScAddress::Contract(id) = &data.contract {
                if id.0 .0[..28] == [0; 28] {
                    return Ok(None);
                }
            }
        }
        if let LedgerKey::ContractData(data) = key.as_ref() {
            if local_fixture_key(&data.key) {
                return Ok(None);
            }
        }
        let key = key.to_xdr_base64(Limits::none()).unwrap();
        let result = self.query(serde_json::json!({"key":key}));
        if let Some(error) = result.get("error") {
            eprintln!("Fork source: {error}");
            return Err(HostError::from((
                soroban_sdk::xdr::ScErrorType::Storage,
                soroban_sdk::xdr::ScErrorCode::MissingValue,
            )));
        }
        let v = &result["entry"];
        if v.is_null() {
            return Ok(None);
        }
        let data =
            LedgerEntryData::from_xdr_base64(v["xdr"].as_str().unwrap(), Limits::none()).unwrap();
        Ok(Some((
            Rc::new(LedgerEntry {
                last_modified_ledger_seq: v["lastModifiedLedgerSeq"].as_u64().unwrap() as u32,
                data,
                ext: LedgerEntryExt::V0,
            }),
            v["liveUntilLedgerSeq"].as_u64().map(|n| n as u32),
        )))
    }
}
fn local_fixture_key(value: &soroban_sdk::xdr::ScVal) -> bool {
    use soroban_sdk::xdr::{ScAddress, ScVal};
    match value {
        ScVal::Address(ScAddress::Contract(id)) => id.0 .0[..28] == [0; 28],
        ScVal::Vec(Some(values)) => values.iter().any(local_fixture_key),
        ScVal::Map(Some(entries)) => entries
            .iter()
            .any(|e| local_fixture_key(&e.key) || local_fixture_key(&e.val)),
        _ => false,
    }
}

#[contracttype]
#[derive(Clone)]
struct Balance {
    amount: i128,
    authorized: bool,
    clawback: bool,
}
fn fund(env: &Env, token: &Address, user: &Address, amount: i128) {
    // Fixture-only mint in the LOCAL snapshot. Deployed SAC code stays intact.
    env.as_contract(token, || {
        env.storage().persistent().set(
            &(symbol_short!("Balance"), user.clone()),
            &Balance {
                amount,
                authorized: true,
                clawback: false,
            },
        )
    });
}
const XLM: &str = "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA";
const USDC: &str = "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75";
const BLND: &str = "CD25MNVTZDL4Y3XBCPCJXGXATV5WUHHOWMYFF4YBEGU5FCPGMYTVG5JY";
fn fork() -> Env {
    let (source, info) = RpcSource::new();
    let mut env = Env::from_ledger_snapshot(SnapshotSourceInput {
        source,
        ledger_info: None,
        snapshot: None,
    });
    env.set_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    let id = env
        .crypto()
        .sha256(&Bytes::from_slice(
            &env,
            b"Public Global Stellar Network ; September 2015",
        ))
        .to_array();
    env.ledger().with_mut(|l| {
        l.protocol_version = info["ledger"]["protocolVersion"].as_u64().unwrap() as u32;
        l.sequence_number = info["ledger"]["sequence"].as_u64().unwrap() as u32;
        l.timestamp = info["timestamp"].as_u64().unwrap();
        l.network_id = id;
        l.max_entry_ttl = 6_312_000;
    });
    env
}
fn register(env: &Env, admin: &Address, treasury: &Address) -> Address {
    #[cfg(not(feature = "wasm-tests"))]
    {
        env.register(Aggregator, (admin.clone(), treasury.clone(), Some(25u32)))
    }
    #[cfg(feature = "wasm-tests")]
    {
        env.register_at(
            &Address::generate(env),
            include_bytes!("../target/wasm32v1-none/release/lumix_aggregator.wasm").as_slice(),
            (admin.clone(), treasury.clone(), Some(25u32)),
        )
    }
}

#[test]
#[ignore = "Read-only mainnet fork; capture with LUMIX_FORK_LIVE=1, then rerun offline"]
fn mainnet_fork_each_protocol_both_modes() {
    let _guard = FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for (protocol, pool, input, _router, _factory, _fee) in [
        (
            Protocol::Soroswap,
            "CAM7DY53G63XA4AJRS24Z6VFYAFSSF76C3RZ45BE5YU3FQS5255OOABP",
            XLM,
            Some("CAG5LRYQ5JVEUI5TEID72EYOVX44TTUJT5BQR2J6J77FH65PCCFAJDDH"),
            None,
            30,
        ),
        (
            Protocol::Aquarius,
            "CA6PUJLBYKZKUEKLZJMKBZLEKP2OTHANDEOWSFF44FTSYLKQPIICCJBE",
            XLM,
            None,
            None,
            10,
        ),
        (
            Protocol::Aquarius,
            "CDRRLXLBXYVENKIUCCTFUT5WW6BW43NJMAPEMGU6VIU74LV6526OLS35",
            "CDTSZ2VWVPLNCTHVFK7Y2KKOYDH32NAZ5KMPZIMP3BJ2WBAVPH6SWECE",
            None,
            None,
            10,
        ),
        (
            Protocol::Aquarius,
            "CBBMQBNHB2FYVZYV7VNHOJHUMTFJLR4PUMRVQYNW6RHIKZO2NQMIBUCV",
            XLM,
            None,
            None,
            10,
        ),
        (
            Protocol::Phoenix,
            "CBHCRSVX3ZZ7EGTSYMKPEFGZNWRVCSESQR3UABET4MIW52N4EVU6BIZX",
            XLM,
            None,
            None,
            50,
        ),
        (
            Protocol::Sushi,
            "CCR2CH4GQVCZHG7CHFVMNANCK45CU5DVKXZIIITDZQAU3CEJZ7RQH2MQ",
            XLM,
            Some("CDMIM23WOUL5CZBKX3GOA3V5R5AMVIMTCP52KCDQORWELAPLJ27WZCHL"),
            Some("CD3KRKGDRVWPXVB3VXLUMQKMX6XZ6Q2H334IVZD4XXNAMKSRVQL5GLYF"),
            3000,
        ),
        (
            Protocol::Comet,
            "CAS3FL6TLZKDGGSISDBWGGPXT3NRR4DYTZD7YOD3HMYO6LTJUVGRVEAM",
            BLND,
            None,
            None,
            30,
        ),
    ] {
        for exact_out in [false, true] {
            for reverse in [false, true] {
                let env = fork();
                let admin = Address::generate(&env);
                let treasury = Address::generate(&env);
                let user = Address::generate(&env);
                let recipient = Address::generate(&env);
                let agg = register(&env, &admin, &treasury);
                let client = AggregatorClient::new(&env, &agg);
                let original_a = Address::from_str(&env, input);
                let original_b = Address::from_str(&env, USDC);
                let (a, b) = if reverse {
                    (original_b.clone(), original_a.clone())
                } else {
                    (original_a.clone(), original_b.clone())
                };
                let p = Address::from_str(&env, pool);
                fund(&env, &a, &user, 1_000_000_000);
                let amount = if protocol == Protocol::Comet {
                    1_000_000
                } else {
                    10_000_000
                };
                // Small target avoids the live Comet ratio caps and unindexed CL ticks.
                let target = if protocol == Protocol::Comet {
                    1_000
                } else {
                    10_000
                };
                let gross = lumix_aggregator::gross_for_net(target, 25).unwrap();
                let req = SwapRequest {
                    sender: user.clone(),
                    recipient: Some(recipient.clone()),
                    token_in: a.clone(),
                    token_out: b.clone(),
                    amount: if exact_out { target } else { amount },
                    limit: if exact_out { amount } else { 1 },
                    deadline: env.ledger().timestamp() + 300,
                    fee_bps: 25,
                    legs: vec![
                        &env,
                        Leg {
                            amount: if exact_out { gross } else { amount },
                            limit: if exact_out { amount } else { 1 },
                            hops: vec![
                                &env,
                                Hop {
                                    protocol: protocol.clone(),
                                    pool: p.clone(),
                                    token_in: a.clone(),
                                    token_out: b.clone(),
                                    output: if exact_out { gross } else { 0 },
                                    limit: if exact_out { amount } else { 1 },
                                    max_spread_bps: Some(10000),
                                    max_price: None,
                                },
                            ],
                        },
                    ],
                };
                signed(&env, &agg, &req, exact_out);
                let result = if exact_out {
                    client.swap_exact_out(&req)
                } else {
                    client.swap_exact_in(&req)
                };
                let cpu = env.cost_estimate().budget().cpu_instruction_cost();
                let memory = env.cost_estimate().budget().memory_bytes_cost();
                assert!(cpu < 100_000_000 && memory < 40 * 1024 * 1024);
                assert!(result.amount_in > 0 && result.amount_in <= amount);
                assert!(result.amount_out > 0);
                if exact_out {
                    assert_eq!(result.amount_out, target);
                }
                assert_eq!(
                    token::Client::new(&env, &a).balance(&user),
                    1_000_000_000 - result.amount_in
                );
                assert_eq!(
                    token::Client::new(&env, &b).balance(&recipient),
                    result.amount_out
                );
                assert_eq!(token::Client::new(&env, &b).balance(&treasury), result.fee);
                assert_eq!(token::Client::new(&env, &a).balance(&agg), 0);
                assert_eq!(token::Client::new(&env, &b).balance(&agg), 0);
                assert_eq!(token::Client::new(&env, &a).allowance(&agg, &p), 0);
                std::println!("{protocol:?} reverse={reverse} exact_out={exact_out} spent={} delivered={} surplus={} cpu={cpu} memory={memory}",result.amount_in,result.amount_out,result.surplus_out);
            }
        }
    }
}

fn signed(env: &Env, agg: &Address, request: &SwapRequest, exact_out: bool) {
    let funding: i128 = if exact_out {
        request.legs.iter().map(|l| l.limit).sum()
    } else {
        request.amount
    };
    env.cost_estimate().budget().reset_unlimited();
    env.mock_auths(&[MockAuth {
        address: &request.sender,
        invoke: &MockAuthInvoke {
            contract: agg,
            fn_name: if exact_out {
                "swap_exact_out"
            } else {
                "swap_exact_in"
            },
            args: (request.clone(),).into_val(env),
            sub_invokes: &[MockAuthInvoke {
                contract: &request.token_in,
                fn_name: "transfer",
                args: (request.sender.clone(), agg.clone(), funding).into_val(env),
                sub_invokes: &[],
            }],
        },
    }]);
    // SDK auth snapshots use a separate diagnostic budget that grows with
    // the whole call tree. Raise the test cap, then check execution usage.
    env.cost_estimate()
        .budget()
        .reset_limits(1_000_000_000, 512 * 1024 * 1024);
}
fn amounts_in(env: &Env, router: &Address, a: &Address, b: &Address, out: i128) -> i128 {
    let values: soroban_sdk::Vec<i128> = env.invoke_contract(
        router,
        &soroban_sdk::Symbol::new(env, "router_get_amounts_in"),
        (out, vec![env, a.clone(), b.clone()]).into_val(env),
    );
    values.get(0).unwrap()
}
fn amounts_out(env: &Env, router: &Address, a: &Address, b: &Address, input: i128) -> i128 {
    let values: soroban_sdk::Vec<i128> = env.invoke_contract(
        router,
        &soroban_sdk::Symbol::new(env, "router_get_amounts_out"),
        (input, vec![env, a.clone(), b.clone()]).into_val(env),
    );
    values.get(1).unwrap()
}
fn phoenix_out(env: &Env, pool: &Address, input: &Address, amount: i128) -> i128 {
    use soroban_sdk::{Map, Symbol, TryFromVal, Val};
    let value: Val = env.invoke_contract(
        pool,
        &Symbol::new(env, "simulate_swap"),
        (input.clone(), amount).into_val(env),
    );
    let m = Map::<Symbol, Val>::try_from_val(env, &value).unwrap();
    i128::try_from_val(env, &m.get(Symbol::new(env, "ask_amount")).unwrap()).unwrap()
}
fn padded(n: i128) -> i128 {
    (n * 10050 + 9999) / 10000
}

#[test]
#[ignore = "Read-only mainnet split/multihop fork, including rollback"]
fn mainnet_fork_split_multihop_and_late_failure() {
    let _guard = FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for out in [false, true] {
        let env = fork();
        env.cost_estimate().budget().reset_unlimited();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let user = Address::generate(&env);
        let dst = Address::generate(&env);
        let agg = register(&env, &admin, &treasury);
        let client = AggregatorClient::new(&env, &agg);
        let a = Address::from_str(&env, XLM);
        let b = Address::from_str(&env, USDC);
        let mid = Address::from_str(&env, BLND);
        let phoenix = Address::from_str(
            &env,
            "CBHCRSVX3ZZ7EGTSYMKPEFGZNWRVCSESQR3UABET4MIW52N4EVU6BIZX",
        );
        let xb = Address::from_str(
            &env,
            "CBIQ5MND5I4AXWFSSDPDVN2X3DPR366YMBXUL6TABGUKMKVGOM2IAZJF",
        );
        let bu = Address::from_str(
            &env,
            "CCCDU62TWI744KFK6COAW2PARPVPXKKE3DBVBUZCFWZOGGD7HZ5YEY3X",
        );
        let aqua = Address::from_str(
            &env,
            "CA6PUJLBYKZKUEKLZJMKBZLEKP2OTHANDEOWSFF44FTSYLKQPIICCJBE",
        );
        let router = Address::from_str(
            &env,
            "CAG5LRYQ5JVEUI5TEID72EYOVX44TTUJT5BQR2J6J77FH65PCCFAJDDH",
        );
        fund(&env, &a, &user, 10_000_000_000);
        fund(&env, &a, &agg, 77);
        fund(&env, &b, &agg, 99);
        let hop = |pool: &Address, i: &Address, o: &Address, output: i128, limit: i128| Hop {
            protocol: if pool == &phoenix {
                Protocol::Phoenix
            } else if pool == &aqua {
                Protocol::Aquarius
            } else {
                Protocol::Soroswap
            },
            pool: pool.clone(),
            token_in: i.clone(),
            token_out: o.clone(),
            output,
            limit,
            max_spread_bps: Some(10000),
            max_price: None,
        };
        let (amount, limit, legs) = if out {
            let net = 10000;
            let gross = lumix_aggregator::gross_for_net(net, 25).unwrap();
            let t1 = 3000;
            let t2 = 2000;
            let t3 = gross - t1 - t2;
            let mid_cap = padded(amounts_in(&env, &router, &mid, &b, t2));
            let root_cap = padded(amounts_in(&env, &router, &a, &mid, mid_cap));
            let aqua_in: u128 = env.invoke_contract(
                &aqua,
                &soroban_sdk::Symbol::new(&env, "estimate_swap_strict_receive"),
                (0u32, 1u32, t3 as u128).into_val(&env),
            );
            let aqua_cap = padded(aqua_in as i128);
            (
                net,
                20000 + root_cap + aqua_cap,
                vec![
                    &env,
                    Leg {
                        amount: t1,
                        limit: 20000,
                        hops: vec![&env, hop(&phoenix, &a, &b, t1, 20000)],
                    },
                    Leg {
                        amount: t2,
                        limit: root_cap,
                        hops: vec![
                            &env,
                            hop(&xb, &a, &mid, mid_cap, root_cap),
                            hop(&bu, &mid, &b, t2, mid_cap),
                        ],
                    },
                    Leg {
                        amount: t3,
                        limit: aqua_cap,
                        hops: vec![&env, hop(&aqua, &a, &b, t3, aqua_cap)],
                    },
                ],
            )
        } else {
            let n1 = 171875000;
            let n2 = 15625000;
            let n3 = 812500000;
            let min1 = phoenix_out(&env, &phoenix, &a, n1) * 9950 / 10000;
            let middle = amounts_out(&env, &router, &a, &mid, n2);
            let min2 = amounts_out(&env, &router, &mid, &b, middle) * 9950 / 10000;
            let estimate: u128 = env.invoke_contract(
                &aqua,
                &soroban_sdk::Symbol::new(&env, "estimate_swap"),
                (0u32, 1u32, n3 as u128).into_val(&env),
            );
            let min3 = estimate as i128 * 9950 / 10000;
            let gross = min1 + min2 + min3;
            (
                n1 + n2 + n3,
                gross - lumix_aggregator::fee(gross, 25).unwrap(),
                vec![
                    &env,
                    Leg {
                        amount: n1,
                        limit: min1,
                        hops: vec![&env, hop(&phoenix, &a, &b, 0, min1)],
                    },
                    Leg {
                        amount: n2,
                        limit: min2,
                        hops: vec![&env, hop(&xb, &a, &mid, 0, 1), hop(&bu, &mid, &b, 0, min2)],
                    },
                    Leg {
                        amount: n3,
                        limit: min3,
                        hops: vec![&env, hop(&aqua, &a, &b, 0, min3)],
                    },
                ],
            )
        };
        let req = SwapRequest {
            sender: user.clone(),
            recipient: Some(dst.clone()),
            token_in: a.clone(),
            token_out: b.clone(),
            amount,
            limit,
            deadline: env.ledger().timestamp() + 300,
            fee_bps: 25,
            legs,
        };
        signed(&env, &agg, &req, out);
        let result = if out {
            client.swap_exact_out(&req)
        } else {
            client.swap_exact_in(&req)
        };
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        let memory = env.cost_estimate().budget().memory_bytes_cost();
        assert!(cpu < 100_000_000 && memory < 40 * 1024 * 1024);
        assert_eq!(
            token::Client::new(&env, &a).balance(&user),
            10_000_000_000 - result.amount_in
        );
        assert_eq!(
            token::Client::new(&env, &b).balance(&dst),
            result.amount_out
        );
        assert_eq!(token::Client::new(&env, &b).balance(&treasury), result.fee);
        assert_eq!(token::Client::new(&env, &a).balance(&agg), 77);
        assert_eq!(token::Client::new(&env, &b).balance(&agg), 99);
        assert_eq!(token::Client::new(&env, &mid).balance(&agg), 0);
        std::println!(
            "split four hops out={out} input={} output={} cpu={cpu} memory={memory}",
            result.amount_in,
            result.amount_out
        );
        let mut bad = req.clone();
        let mut last = bad.legs.get(2).unwrap();
        let mut h = last.hops.get(0).unwrap();
        if out {
            last.limit = 1;
            h.limit = 1;
        } else {
            last.limit *= 2;
            h.limit = last.limit;
        }
        last.hops.set(0, h);
        bad.legs.set(2, last);
        if out {
            bad.limit = bad.legs.iter().map(|l| l.limit).sum();
        } else {
            let g = bad.legs.iter().map(|l| l.limit).sum();
            bad.limit = g - lumix_aggregator::fee(g, 25).unwrap();
        }
        let watched = [&user, &dst, &treasury, &agg, &phoenix, &xb, &bu, &aqua];
        let assets = [&a, &b, &mid];
        let before: std::vec::Vec<_> = assets
            .iter()
            .flat_map(|t| {
                watched
                    .iter()
                    .map(|who| token::Client::new(&env, t).balance(who))
            })
            .collect();
        signed(&env, &agg, &bad, out);
        assert!(if out {
            client.try_swap_exact_out(&bad).is_err()
        } else {
            client.try_swap_exact_in(&bad).is_err()
        });
        let after: std::vec::Vec<_> = assets
            .iter()
            .flat_map(|t| {
                watched
                    .iter()
                    .map(|who| token::Client::new(&env, t).balance(who))
            })
            .collect();
        assert_eq!(before, after);
    }
}

#[test]
#[ignore = "Deployed Phoenix bootstrap floor rejects a positive simulation quote"]
fn mainnet_fork_phoenix_bootstrap_floor_is_atomic() {
    let _guard = FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = fork();
    env.cost_estimate().budget().reset_unlimited();
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let user = Address::generate(&env);
    let agg = register(&env, &admin, &treasury);
    let client = AggregatorClient::new(&env, &agg);
    let a = Address::from_str(&env, XLM);
    let b = Address::from_str(&env, USDC);
    let p = Address::from_str(
        &env,
        "CBENABXP6C4C7WG6KB7JQOTDS5GIIXF3IX3PIYNZFCDZDWUHITO2HZ4S",
    );
    let quote = phoenix_out(&env, &p, &a, 171875000);
    assert!(quote > 0);
    fund(&env, &a, &user, 1_000_000_000);
    let request = SwapRequest {
        sender: user.clone(),
        recipient: None,
        token_in: a.clone(),
        token_out: b.clone(),
        amount: 171875000,
        limit: 1,
        fee_bps: 25,
        deadline: env.ledger().timestamp() + 300,
        legs: vec![
            &env,
            Leg {
                amount: 171875000,
                limit: 1,
                hops: vec![
                    &env,
                    Hop {
                        protocol: Protocol::Phoenix,
                        pool: p,
                        token_in: a.clone(),
                        token_out: b.clone(),
                        output: 0,
                        limit: 1,
                        max_price: None,
                        max_spread_bps: None,
                    },
                ],
            },
        ],
    };
    signed(&env, &agg, &request, false);
    assert!(matches!(
        client.try_swap_exact_in(&request),
        Err(Err(soroban_sdk::InvokeError::Contract(337)))
    ));
    assert_eq!(token::Client::new(&env, &a).balance(&user), 1_000_000_000);
    assert_eq!(token::Client::new(&env, &a).balance(&agg), 0);
    assert_eq!(token::Client::new(&env, &b).balance(&agg), 0);
}
