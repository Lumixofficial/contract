# Lumix

Isolated perpetuals and fully collateralized P2P European options on Stellar/Soroban. Both products use the same `lumix-oracle` instance. Users submit requests and escrow funds; keepers validate them later against immutable prices for the requested timestamps.

## Contracts

| Contract | Role |
| --- | --- |
| `lumix-factory` | Market registry and discovery |
| `lumix-pool` | LP liquidity, shares, swaps, reserves, and trader collateral accounting |
| `lumix-exchange` | Perp request escrow, cancellation, and keeper execution |
| `lumix-market` | Perp positions, fees, risk checks, and liquidation |
| `lumix-options` | Option order escrow, keeper matching, and European settlement |
| `lumix-oracle` | Immutable timestamped prices and an optional external source adapter |

`lumix-core` contains shared math. `lumix-interfaces` contains cross-contract clients without linking contract implementations into Wasm. Referral, staking/token, and router contracts were removed. Users call pools, the exchange, and options directly.

## Perp requests

1. The user calls `exchange.create_order(...)`. The exchange accepts only markets registered in its permanently bound factory and pins the escrow token at submission. The contract assigns an ID and ledger timestamp, records the request, and holds opening collateral plus any execution fee in exchange escrow. No position is opened or closed yet. Close requests leave existing position collateral in place.
2. A price publisher calls `oracle.set_price_at(caller, asset, timestamp, min, max)` for the request's `price_timestamp`. Supply observations for the index and both pool assets unless an asset has a configured fixed price.
3. A keeper calls `exchange.execute_order(keeper, id)`. It validates the request, reads exact historical observations, and atomically updates the position. Opening collateral transfers from exchange escrow to the pool only when validation succeeds.

Disabling a factory market blocks increases but permits closing existing positions.

Missing prices, invalid signatures, slippage, inadequate margin, insufficient liquidity, or other failures leave the request and its escrow unchanged. The user can cancel pending requests and recover remaining escrow, including while frozen or paused. Executed requests cannot execute or refund twice.

Only isolated orders are accepted by the exchange. Each market uses its `short_token` as collateral. There can be one pending request per user, market, and direction. Amendments select the current ledger timestamp as their new price timestamp. Limits/TP/SL are evaluated at that timestamp. TWAP slices use `price_timestamp`, then advance it by the interval after each fill. Opening collateral and execution fees are consumed once, on the first successful slice.

Perp fills still apply the configured min/max spread, price impact, fees, and leverage rules. The immutable oracle observation is the shared input, not a promise that every trader receives an identical final fill. Funding and borrow accounting accrue when validation runs. A request older than the last position action cannot execute after that action, including liquidation.

Trader collateral is tracked separately from LP liquidity. Pending collateral is never part of pool assets, and active trader collateral is excluded from LP valuations and withdrawals.

## European options

- The admin enables supported base/quote pairs and exact expiry timestamps, up to 180 days ahead.
- Buyers escrow their quoted premium. Call writers escrow the full base amount; put writers escrow the full strike notional in quote tokens.
- The off-chain orderbook finds two matching requests. A keeper calls `options.match_orders(caller, buy_id, sell_id)`. Both request timestamps must resolve through the shared oracle. The older order still sets the agreed premium; the oracle supplies the historical underlying prices, not an option-pricing model.
- Unmatched orders can be cancelled and refunded. Matched positions have no early-close or early-exercise method.
- At expiry, publish the base and quote observations for **the exact expiry second**, then call `options.settle(position_id)`. The base/quote ratio is computed from those two USD prices. Settlement releases the intrinsic payoff, remaining collateral, and writer premium exactly once.

Settlement remains available if options or their series are disabled. Anyone may submit the deterministic settlement transaction after expiry. **A keeper/automation service must send that transaction. Contracts do not run on a timer.** No holder close request is required, and later prices cannot replace the expiry price.

## Liquidation

Publish the index and pool-asset observations for a recent timestamp, then call:

```text
market.liquidate(account, is_long, liquidator, timestamp)
```

The caller must be the market admin or an authorized market keeper. Liquidation is independent of open/close requests. The timestamp cannot precede the position's last action, be in the future, or be more than 600 seconds old. The contract checks maintenance margin and rejects healthy positions. A pending close does not prevent liquidation; its unused execution fee remains cancellable.

## Oracle behavior

- Keys are `(asset address, Unix timestamp in seconds)`. The first successful publication wins permanently. Subsequent authorized writes for the same key are ignored, including conflicting values. Assets and timestamps remain independent.
- Future timestamps, nonpositive/reversed prices, arithmetic overflow, unauthorized publishers, and paused-oracle operations are rejected.
- `quote_at(asset, timestamp)` resolves exactly that timestamp. It never substitutes the latest price and does not expire an existing historical observation because execution was delayed.
- `quote(asset)` retains freshness checks for live LP operations and views. Publish fresh prices when those operations need them; no continuous publishing loop is required for pending trades.
- The deviation guard applies to nearby, forward observations. It does not force fabricated intermediate prices for historical backfills or gaps longer than the live-price age limit.
- Historical observations use persistent storage with TTL extension. Operators must retain/restore archived contract state as needed; expiry does not authorize replacing an observation.

Manual mode trusts admins, keepers, and the configured feeder to supply truthful prices. Immutability prevents rewriting an accepted price; it cannot prove that the first price was honest. The admin can also select the source for future observations.

For an explicit accounting peg, call `set_fixed_price(asset, price)` before any observations exist for that asset. For example, USDC at `100000000000000` treats it permanently as $1. This ignores depegs and is optional. Without a peg, the quote asset needs a timestamped price too. A fixed price cannot later be changed.

## External oracle adapter

Configure `oracle.set_source(Some(adapter_address))`. The adapter implements the interface in `lumix-interfaces`:

```rust
fn price_at(env: Env, asset: Address, timestamp: u64) -> OraclePrice;
// OraclePrice { min, max, timestamp }, USD prices scaled by 10^14
```

During request validation, liquidation, or option settlement, `quote_at` fetches any missing observation from this contract, verifies its timestamp and bounds, and caches it immutably. This removes the separate manual publication step. Previously stored observations always win. Manual publication of new observations is blocked while a source is configured.

The adapter must support the requested historical second or fail. Do not relabel a latest quote as a historical price. Soroban calls another on-chain contract here, not an HTTP API. No production provider adapter is included yet. Source failure rolls back validation, including any cache writes made during that failed transaction.

## Build and verification

```bash
cargo test --workspace
stellar contract build
cargo test -p lumix-test-suites --features wasm-tests flow_tests
python3 scripts/test-mutations.py
```

See [fuzz/README.md](fuzz/README.md) for runnable libFuzzer targets and [docs/verification.md](docs/verification.md) for results and limits. The Wasm suite runs the compiled six-contract system against the Soroban host, including real token transfers, strict auth tests, and per-transaction resource checks. Property tests cover request sequences, escrow conservation, oracle history, and independent option-payoff calculations.

## Deployment and compatibility

This is a **new deployment**, not an in-place upgrade of the existing testnet contracts. Constructor/function signatures and stored records changed. Existing contracts have no upgrade entrypoint. Keep the old deployment available to resolve any existing funds and positions.

Use the updated `scripts/deploy.sh.example`, refresh any local copies, and choose a new state file/market slug. It deploys six contracts, binds the exchange to the factory, passes the same oracle to pool/market/exchange/options, and authorizes keepers on the market as well as exchange/options/oracle. The existing deployment manifests remain historical records and were not rewritten.

The listener/frontend/keeper must adopt the changed ABIs, request timestamps, and direct pool/exchange calls. There is no `options.set_settlement` bypass or price argument on `exchange.execute_order` anymore. Nothing in this change deploys contracts or starts a keeper service.

## Scales

- Token amounts and USD notionals: 7 decimals.
- Oracle prices and option strikes: **14 decimals**, `$1 = 100000000000000`.
- Funding/borrow indices: 12 decimals.

The old `PRICE_SCALE` constant incorrectly used 13 decimals. This version corrects it to match the documented scale and deployment inputs; old encoded prices must not be reused without checking their scale.

## Spot DEX aggregator

The independent [spot aggregator contract](contracts/lumix-aggregator) executes exact-input and exact-output routes across Soroswap, Aquarius, Phoenix, Sushi, and Comet in one transaction. It supports splits, mixed multihop paths, signed limits, optional destinations, output-token fees defaulting to zero, and refunds. It uses a separate SDK 29 workspace without changing the existing six contracts.

See the [ABI, fee rules, protocol behavior, twelve examples, and testing guide](docs/spot-aggregator.md). Run its tests with `cargo test --manifest-path contracts/lumix-aggregator/Cargo.toml`.
