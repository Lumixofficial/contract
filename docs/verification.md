# Contract flow verification

Verified locally on 2026-10-08. Changes are uncommitted. No contracts were deployed and no keeper service was started.

## Results

| Check | Result |
| --- | --- |
| `cargo test --workspace` | 146 tests passed, zero failures or ignored tests |
| Compiled Wasm flow suite | 24 tests passed against the Soroban host |
| `stellar contract build` | All six contracts built successfully |
| Oracle libFuzzer target | 36,096 runs, 61 seconds, no failure |
| Perp libFuzzer target | 1,281 runs, 61 seconds, no failure |
| Option libFuzzer target | 8,346 runs, 61 seconds, no failure |
| Focused mutation checks | Ten deliberate faults caught by failing tests |
| `cargo clippy --workspace --all-targets` | Completed with warnings |
| Formatting and diff whitespace checks | Passed |
| Deployment script | Bash syntax and six-contract JSON manifest checked offline |

The three lifecycle property tests also run 64 generated cases each. Existing financial math property tests remain in `lumix-core`. LibFuzzer runs used coverage instrumentation with sanitizers disabled because this macOS nightly's thread sanitizer has an ABI mismatch. See [fuzzing instructions](../fuzz/README.md).

Clippy warnings concern long Soroban argument lists, unused fixture fields, and the existing deprecated share-transfer event API. They are not test failures.

## Verified behavior

- Pool, market, exchange, and options use the same configured oracle; a market rejects a pool using another oracle and an exchange rejects incompatible market wiring.
- Unregistered markets cannot receive order escrow. The exchange binds permanently to the factory; refunds use the token recorded at submission even if market metadata changes. Disabling a registry market blocks increases while preserving closes.
- Perp submission records the request timestamp and holds collateral/fees in exchange escrow without changing positions or LP balances.
- Validation uses the exact request timestamp, including after a delay and when a different latest price exists.
- Different users and products sharing a timestamp use the first accepted observation. Conflicting retries cannot overwrite it or emit another price update.
- Historical reads reject missing/future observations. Asset keys remain independent, and out-of-order backfills do not roll the latest pointer backward.
- Keeper signatures and roles are required. Execution needs no second trader signature; users cannot call the market directly to bypass requests.
- Missing prices, source failure, slippage, and failed validation roll back transfers, positions, fees, reserves, and partial oracle-cache writes.
- Pending requests cancel with full remaining escrow refunds, including when frozen or paused. Replays cannot pay fees, open positions, settle options, or refund twice.
- Liquidation requires an authorized caller, a recent exact observation, and a maintenance-margin breach. It operates independently of pending closes and cannot liquidate a healthy position.
- Option matching requires both request timestamps. Unmatched orders refund; matched positions cannot close early. Expiry settlement uses the exact expiry observation and releases all collateral/premium once.
- Dynamic quote assets need prices at the same timestamp. Option settlement calculates the base/quote ratio instead of assuming a $1 quote asset.
- Calls and puts match independent payoff calculations, and balances across users, pool, exchange, options, treasury, and keeper conserve the minted token supply.
- Active trader collateral is excluded from LP valuation and withdrawals. Oracle history receives persistent TTL extension.
- An on-chain source fixture resolves missing prices during validation and settlement without a separate publication transaction. Invalid timestamps/prices fail closed.

## Wasm transaction resources

The SDK's invocation-resource enforcement stays enabled. Test-only diagnostic/auth snapshot overhead is unlimited; assertions separately enforce at most 100 million CPU instructions and 40 MiB of metered contract memory per measured transaction.

| Operation | CPU instructions | Memory bytes |
| --- | ---: | ---: |
| Publish price | 835,387 | 1,387,684 |
| Execute perp open | 17,186,083 | 32,183,599 |
| Execute perp close | 12,159,130 | 23,278,171 |
| Liquidate perp | 9,740,530 | 18,141,042 |
| Validate option match | 3,541,064 | 6,596,576 |
| Settle option | 2,752,815 | 4,207,882 |
| Perp open resolving both dynamic assets | 17,626,871 | 32,316,312 |

These are local host measurements for the tested fixtures, not live-network fee estimates. The six production contracts execute compiled Wasm in these tests; token transfers use the host's Stellar Asset Contract implementation, and external-source adapters are test fixtures.

## Contract sizes

| Contract | Optimized bytes |
| --- | ---: |
| Oracle | 15,634 |
| Pool | 25,755 |
| Market | 39,649 |
| Exchange | 16,181 |
| Factory | 5,042 |
| Options | 22,004 |

## Deployment boundaries

The ABIs, records, and corrected 14-decimal price scale require a new deployment and corresponding frontend/listener/keeper updates. Existing manifests and deployed contracts were left untouched.

European expiry needs a keeper or automation transaction. The contracts cannot execute themselves on a timer. No real external provider adapter is included; the source interface and its validation path are ready for one. Manual prices still trust the first publisher, and admins select future sources. These tests establish the requested mechanics, not the honesty of supplied prices or a full security audit.

The testing tools follow Stellar's [fuzzing](https://developers.stellar.org/docs/build/guides/testing/fuzzing) and [mutation-testing](https://developers.stellar.org/docs/build/guides/testing/mutation-testing) guidance.
