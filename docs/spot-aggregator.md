# Lumix spot swap aggregator

One new contract: `contracts/lumix-aggregator`. It does not depend on the existing perpetuals, oracle, pool, factory, exchange, or options contracts. Its separate Rust workspace pins SDK 29.0.1 because the live network uses protocol 29 and SDK 27's VM cannot be resolved alongside SDK 29's VM. Build and test it with its own manifest.

The quote service searches pools and computes amounts off chain. The contract executes an explicitly signed route in one Soroban invocation. The only entry points that move user funds are `swap_exact_in(request)` and `swap_exact_out(request)`.

## What the contract guarantees

* Only the sender can authorize spending. Authorization covers the entire request, including recipient, fee rate, all pools, protocol selectors, token addresses, amounts, limits, and deadline.
* Exact input spends the declared input allocations and delivers at least the aggregate **net** minimum after the aggregator fee.
* Exact output delivers the declared **net** output to the destination and spends at most the total input cap.
* Each hop and each leg has its own enforced output floor or input cap. Split allocations must conserve input or gross output, respectively.
* Pool output is measured with token balance changes. A pool's reported return value cannot satisfy a minimum by itself.
* Any failure reverts the whole invocation, including earlier swaps, input collection, fees, refunds, and Comet approvals.
* Each signed hop chooses a pool address and protocol. The contract reads its token order and checks both route assets before funding. It calls that pool directly, without a registry, router, or factory configuration.
* Unused input and intermediate tokens go back to the **sender**. Exact-output surplus goes back to the sender even when the destination is someone else.
* Tokens already sitting on the aggregator are preserved. They cannot satisfy a new swap's minimum or be taken as refunds.
* Expired requests and fee changes fail before funding. No persistent user allowance remains after a Comet swap.

The entry point supports one leg or a split of up to 8 legs. Each leg supports 1 to 4 hops, including mixed protocols. A pool may occur only once across the entire request, and a leg cannot cycle through a token. These rules match the quote service's independent pool calculations. These are structural limits, not promises that every 32-hop transaction fits network resource limits. Always simulate the complete operation.

## Protocols

| Protocol | Execution target | Exact input | Exact output |
| --- | --- | --- | --- |
| Soroswap | Pool directly | Read reserves, transfer input, `swap` | Compute required input from reserves, `swap` |
| Aquarius | Pool directly, indexes from `get_tokens` | `swap` | Native `swap_strict_receive` |
| Phoenix | Pool directly | `swap` | Bounded exact input, output floor = target |
| Sushi | Pool directly, direction from `token0`/`token1` | `swap` with current oracle hints | Bounded exact input, output floor = target |
| Comet | Pool directly | `swap_exact_amount_in` | Native `swap_exact_amount_out` |

The Sushi adapter uses positive-amount pool swaps, retaining the existing bounded-input delivery behavior. Phoenix only offers a fixed-input swap. The aggregator provides fixed-output **delivery** for both by spending the signed hop input cap as an exact input and requiring at least the target. It sends precisely the requested net output to the destination and refunds surplus to the sender. This cannot dynamically reduce the input on those two protocols if prices improve. The UI labels these hops `bounded-exact-in`.

For native exact output, Soroswap computes the minimum input from the current pair reserves and its rounded 0.3% fee; Aquarius queries its strict-receive estimator. Both validate the required input against the signed cap in the same invocation. Soroswap transfers that precise input directly to the pair. Aquarius authorizes its initial pull of the input cap, then the pool refunds its unused portion. The signed input cap still controls spending. This is not an on-chain routing or price search. Comet approves only the signed cap and clears the unused allowance after execution.

Protocol ABIs and Mainnet address references are in [`../../spot-dex-aggregator-tx-maker`](../../spot-dex-aggregator-tx-maker/README.md). Primary references: [Soroswap core](https://github.com/soroswap/core), [Phoenix contracts](https://github.com/Phoenix-Protocol-Group/phoenix-contracts), [Comet contracts](https://github.com/CometDEX/comet-contracts-v1). The deployed Wasm is tested by the local fork suite, not replaced with protocol mocks in those tests.

## Fees

Fees are in the output token. `fee_bps` uses basis points: 0 = 0%, 25 = 0.25%, 100 = 1%. The constructor takes `Option<u32>` and defaults to 0 when omitted/null. Valid range is 0 through 9999.

For exact input, `fee = floor(actualGrossOutput * fee_bps / 10000)`. The destination gets `actualGrossOutput - fee`. The aggregate minimum is a net minimum. Per-leg and per-hop minima are gross amounts.

For exact output, the service allocates the minimal gross output `g` such that `g - floor(g * fee_bps / 10000) = requestedNetOutput`. The fee is `g - requestedNetOutput`. Surplus beyond `g` is refunded to the sender and is not charged another fee.

Integer rounding matters. With a requested net output of 1 base unit and a 1% fee, the minimal gross is 1 and the fee is 0. The contract and TypeScript service use the same formula, with overflow checks.

The signed request's `fee_bps` must equal `get_config().fee_bps`. Read configuration before quoting and pass that rate to the quote service. If the rate changes before execution, obtain a new quote. The treasury needs a trustline for classic output assets if it is a G-account, even when the end user's trustline is already present.

## Deployment and administration

```sh
cd contracts/lumix-aggregator
stellar contract build
# The constructor runs atomically during deployment.
stellar contract deploy --wasm target/wasm32v1-none/release/lumix_aggregator.wasm \
  --network testnet --source admin -- \
  --admin ADMIN_ADDRESS --treasury TREASURY_ADDRESS --fee_bps null
```

Do not reuse Mainnet token/pool/router addresses on Testnet. The examples below refer to Mainnet pool identities; a Testnet swap needs pool and token addresses deployed on Testnet.

`get_config()` returns `{ admin, treasury, fee_bps }`.

`set_fee(fee_bps, treasury)` requires the existing admin's authorization. It changes both values atomically and emits `FeeChanged`.

Deployment with the constructor arguments completes the contract setup. There is no `set_pool`, `get_pool`, or pool registration transaction. No other Lumix contracts or upfront liquidity deposits are required.

Each hop supplies `pool` and `protocol`, alongside its input/output tokens and signed amount limits. Protocol values in the API/Blux native JSON use `{ "tag": "Soroswap" }`, with `Aquarius`, `Phoenix`, `Sushi`, and `Comet` as the other tags. The encoder converts this to the Soroban enum vector.

| Protocol | Details read directly from the supplied pool |
| --- | --- |
| Soroswap | `token_0`, `token_1`, `get_reserves`; fixed rounded 0.3% input fee |
| Aquarius | `get_tokens` for token indexes; strict-receive estimate when needed |
| Phoenix | `query_config` for token addresses and the current pool fee |
| Sushi | `token0`, `token1`, `get_oracle_hints` |
| Comet | `get_tokens`; swap math remains inside the pool |

Pool selection is permissionless. A protocol tag selects an ABI adapter, not proof of official protocol membership. The quote service or caller must choose the intended pool. Token-membership checks, exact transfer/approval authorization, signed limits, measured balance changes, refunds, and atomic rollback remain enforced. Wrong protocol ABIs or incompatible pools fail the invocation.

Configure `AGGREGATOR_CONTRACT_ID` in the API and quote worker after deployment. The quote encoder includes the protocol in every hop, and the API and interface check it against the displayed route. Old requests without `protocol` are incompatible with this ABI and must be regenerated. The constructor and fee configuration are unchanged.

The constructor configuration extends its TTL on use. There are no persistent pool-registry entries to maintain.

## Request ABI

All monetary amounts in JSON are decimal **integer strings**, in token base units. For a 7-decimal token, 10 tokens = `"100000000"`. Do not send floats. Token references are SAC/SEP-41 contract addresses, not symbols or asset issuers. `deadline` is an integer string containing Unix seconds, never milliseconds.

```ts
interface SwapRequest {
  sender: Address;
  recipient: Address | null; // null means sender
  token_in: Address;
  token_out: Address;
  amount: i128;
  limit: i128;
  deadline: u64;
  fee_bps: u32;
  legs: Leg[];
}
interface Leg { amount: i128; limit: i128; hops: Hop[]; }
interface Hop {
  protocol: { tag: "Soroswap" | "Aquarius" | "Phoenix" | "Sushi" | "Comet" };
  pool: Address;
  token_in: Address;
  token_out: Address;
  output: i128;
  limit: i128;
  max_spread_bps: u32 | null;
  max_price: i128 | null;
}
```

| Field | Exact input | Exact output |
| --- | --- | --- |
| Request `amount` | Total input spent | Exact net output delivered |
| Request `limit` | Total minimum net output | Maximum total input |
| Leg `amount` | Input allocated to this leg | Gross output allocated to this leg |
| Leg `limit` | Minimum gross output of this leg | Maximum first-token input for this leg |
| Hop `output` | Must be 0 | Exact output target for this hop |
| Hop `limit` | Minimum gross output of this hop | Maximum input for this hop |

For exact input, leg input amounts must sum to request input. Each last hop's minimum must equal its leg's minimum. Net output calculated from the sum of leg minima must cover the request minimum. Every intermediate hop uses the actual preceding hop's output as its input, so favorable price movement propagates through the route.

For exact output, leg gross output allocations must sum to the fee-adjusted gross target. Leg input caps must sum to at most the request cap. Each first hop cap equals the leg input cap. Every intermediate hop target must equal the following hop's input cap. Each final hop target equals the leg gross output allocation.

The quote engine builds exact-output targets backwards. If hop 2 needs up to 50 intermediate tokens, hop 1 buys 50. If hop 2 actually spends 48, the unused 2 intermediate tokens are refunded to the payer. The displayed root input cost includes funding those intermediate caps, and the interface warning makes this visible. It does not pretend an intermediate-token refund is a refund in the original input token.

`max_spread_bps` is forwarded to Phoenix only. `max_price` is forwarded to Comet only, in its native fixed-point spot-price units. Null uses Phoenix's pool default or Comet's unrestricted spot-price cap. Other protocols use the signed amount floors/caps. Off-chain price-impact percentages are display estimates, not an independently verifiable on-chain oracle. Do not claim an arbitrary percentage is enforced across every protocol without such an oracle. Signed minima/maxima are the universal execution protection.

## Examples

### 1. Single exact input

```sh
cd ../spot-dex-aggregator
npm run quote -- XLM USDC 100 --no-split --slippage-bps 50 --json
```

Sell exactly 100 XLM, through one route. The route can contain one pool or several pools depending on `--hops`. Use `--hops 1` for direct routes only.

### 2. Split exact input

```sh
npm run quote -- XLM USDC 100 --max-splits 3 --hops 3 --json
```

The best route can divide the 100 XLM among distinct pools. Each leg has its own input allocation, path, expected output, minimum, price impact, pool ledger, and coverage.

### 3. Single exact output

```sh
npm run quote -- --asset-in XLM --asset-out USDC --amount-out 20 --no-split --json
```

Buy exactly 20 **net** USDC. Plans are ranked by lowest input cost, not highest output. Each plan includes `maximumInput` and `maximumInputRaw`.

### 4. Split exact output with a fee

```sh
npm run quote -- --asset-in XLM --asset-out USDC --amount-out 20 \
  --fee-bps 25 --max-splits 3 --slippage-bps 50 --json
```

The legs split the gross output needed to deliver 20 USDC after the 0.25% fee. Exact-output split search defaults to 16 allocation steps; increase `--split-steps` to improve the bounded search at a higher CPU cost.

### 5. An unsigned request for the sender

```sh
npm run quote -- XLM USDC 100 --json --sender USER_ADDRESS \
  --contract AGGREGATOR_ADDRESS --fee-bps 0
```

The JSON response includes `interfaceQuotes` and `executionRequests`. Each execution artifact contains `schemaVersion`, `contractId`, `mode`, `function`, `request`, and `display`. No signing or submission occurs.

### 6. Exact output to another destination

```sh
npm run quote -- --asset-in XLM --asset-out USDC --amount-out 20 --json \
  --sender USER_ADDRESS --recipient DESTINATION_ADDRESS \
  --contract AGGREGATOR_ADDRESS --fee-bps 0
```

The sender pays. The destination receives exactly 20 USDC. Unspent input, intermediate balances, and excess output are returned to the sender. The destination does not need to authorize the swap, but a G-account must have the output asset's trustline. A G-account sender also needs trustlines for any classic intermediate token refund and any fallback output surplus, even when the destination is different.

### 7. Quote and build an operation in TypeScript

```ts
import { Aggregator, compileExecution, buildSwapOperation } from './src/aggregator/index.js';
// store/rpc/settings are the same initialized services used by quoteCli.
const quote = await new Aggregator(store, rpc, settings).quote({
  assetIn: 'XLM', assetOut: 'USDC', amountIn: '100',
  feeBps: onChainConfig.fee_bps, slippageBps: 50,
});
if (!quote.bestQuote) throw new Error('No valid route');
const artifact = compileExecution(quote.bestQuote, {
  sender: userAddress, recipient: destinationAddress,
  contractId: aggregatorAddress, feeBps: onChainConfig.fee_bps,
});
const operation = buildSwapOperation(artifact.contractId, artifact.request, artifact.mode);
```

`compileExecution` rejects preliminary quotes, expired quotes, wrong fees, inconsistent totals, cyclic paths, repeated pools, and invalid addresses/amounts. `encodeSwapRequest` encodes the actual Rust struct ABI with address, i128, u32, u64, vectors, maps, and options. A cross-language fixture test decodes its XDR into the Rust request type.

### 8. Prepare and submit one transaction

```ts
import { TransactionBuilder, Networks, rpc } from '@stellar/stellar-sdk';
const server = new rpc.Server(rpcUrl);
const account = await server.getAccount(userAddress);
const secondsLeft = Number(artifact.request.deadline) - Math.floor(Date.now() / 1000);
if (secondsLeft <= 0) throw new Error('Quote expired');
const transaction = new TransactionBuilder(account, {
  fee: '100', networkPassphrase: Networks.PUBLIC,
}).addOperation(operation).setTimeout(secondsLeft).build();
const prepared = await server.prepareTransaction(transaction);
// userAddress is both transaction source and swap sender in this example.
// The wallet signs the PREPARED transaction, including its resource footprint.
const signed = await wallet.signTransaction(prepared.toXDR(), {
  networkPassphrase: Networks.PUBLIC,
});
const response = await server.sendTransaction(
  TransactionBuilder.fromXDR(signed.signedTxXdr, Networks.PUBLIC),
);
// Poll getTransaction(response.hash) for SUCCESS/FAILED. PENDING is not success.
```

Keep this as one `invokeHostFunction` operation calling the aggregator. Do not construct separate top-level pool swap transactions. If a different account sponsors/sources the transaction, the sender also needs to sign the Soroban authorization entries. A transaction source signature from the sponsor cannot replace the sender's authorization.

### 9. Explicit Phoenix impact and Comet price limits

```ts
const artifact = compileExecution(plan, {
  contractId: aggregatorAddress, sender: userAddress,
  feeBps: onChainConfig.fee_bps,
  hopLimits: {
    [phoenixPool]: { maxSpreadBps: 100 }, // Phoenix's native spread definition
    [cometPool]: { maxPriceRaw: '1800000000' }, // pool fixed-point units
  },
});
```

These caps are pool-specific and are not interchangeable with UI price-impact percentages.

### 10. A mixed route and split in raw contract JSON

The following uses the amounts from the supplied historical 100 XLM quote, with a permissive intermediate-hop minimum of one base unit. It illustrates the ABI; **re-quote before executing**. Replace `sender` and `deadline` with the actual sender and fresh Unix deadline. Pool approval is required first.

```json
{
  "sender": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
  "recipient": null,
  "token_in": "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA",
  "token_out": "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75",
  "amount": "1000000000",
  "limit": "190632905",
  "deadline": "1791897361",
  "fee_bps": 0,
  "legs": [
    {
      "amount": "171875000", "limit": "33768702",
      "hops": [{
        "protocol": {"tag": "Phoenix"}, "pool": "CBENABXP6C4C7WG6KB7JQOTDS5GIIXF3IX3PIYNZFCDZDWUHITO2HZ4S",
        "token_in": "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA",
        "token_out": "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75",
        "output": "0", "limit": "33768702", "max_spread_bps": 100, "max_price": null
      }]
    },
    {
      "amount": "15625000", "limit": "3003121",
      "hops": [
        {
          "protocol": {"tag": "Soroswap"}, "pool": "CBIQ5MND5I4AXWFSSDPDVN2X3DPR366YMBXUL6TABGUKMKVGOM2IAZJF",
          "token_in": "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA",
          "token_out": "CD25MNVTZDL4Y3XBCPCJXGXATV5WUHHOWMYFF4YBEGU5FCPGMYTVG5JY",
          "output": "0", "limit": "1", "max_spread_bps": null, "max_price": null
        },
        {
          "protocol": {"tag": "Soroswap"}, "pool": "CCCDU62TWI744KFK6COAW2PARPVPXKKE3DBVBUZCFWZOGGD7HZ5YEY3X",
          "token_in": "CD25MNVTZDL4Y3XBCPCJXGXATV5WUHHOWMYFF4YBEGU5FCPGMYTVG5JY",
          "token_out": "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75",
          "output": "0", "limit": "3003121", "max_spread_bps": null, "max_price": null
        }
      ]
    },
    {
      "amount": "812500000", "limit": "153861082",
      "hops": [{
        "protocol": {"tag": "Aquarius"}, "pool": "CA6PUJLBYKZKUEKLZJMKBZLEKP2OTHANDEOWSFF44FTSYLKQPIICCJBE",
        "token_in": "CAS3J7GYLGXMF6TDJBBYYSE3HQ6BBSMLNUQ34T6TZMYMW2EVH34XOWMA",
        "token_out": "CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75",
        "output": "0", "limit": "153861082", "max_spread_bps": null, "max_price": null
      }]
    }
  ]
}
```

### 11. Exact-output raw shape for two hops

For a 0% fee and desired net output of 100 units, a leg can cap initial input at 120 units, target 110 intermediate units, then buy 100 output units with an intermediate input cap of 110:

```text
request: amount=100, limit=120
leg:     amount=100, limit=120
hop 1:   A -> B, output=110, limit=120
hop 2:   B -> C, output=100, limit=110
```

If hop 1 spends 105 A and hop 2 spends 102 B, the sender is refunded 15 A and 8 B. The recipient receives exactly 100 C. If either hop needs more than its cap or delivers too little, both swaps revert.

### 12. Choosing a quote for the interface

Use `response.interfaceQuotes` for display, or `toInterfaceQuote(plan)` for a selected plan. It provides decimal strings and raw amounts, leg allocation basis, paths/symbols, per-hop protocol/pool/token addresses, observed ledger, coverage, fee, and execution mode. Root output/minimum are **net**; leg/hop outputs are **gross**. Exact-output share percentages describe the gross output allocation, not input shares.

The interface data includes `settlement.refundTokens` with token address, type, symbol, asset code, and issuer where metadata is known. Use it to check a G-account sender's trustlines before choosing an exact-output path. Contract accounts hold SAC balances directly and do not need classic trustlines.

Concentrated pool quotes currently cover the indexed current tick interval. Routes needing unavailable tick crossings are excluded. Weighted quotes remain labeled estimates. The contract's signed bounds protect execution, and full-transaction simulation determines whether the exact selected route is executable on current state.

## Tests and reproduction

```sh
# From contract repository root:
cargo test --manifest-path contracts/lumix-aggregator/Cargo.toml
# Build Wasm using the required Stellar build settings:
cd contracts/lumix-aggregator
stellar contract build
cargo test --features wasm-tests --lib

# Capture deployed Mainnet code and state through read-only RPC, execute locally:
LUMIX_FORK_LIVE=1 cargo test --test mainnet_fork -- --ignored --nocapture
# Rerun from the captured data with no RPC calls:
cargo test --test mainnet_fork -- --ignored --nocapture
# Repeat with the aggregator Wasm itself:
cargo test --features wasm-tests --test mainnet_fork -- --ignored --nocapture

# Quote project:
cd ../../../spot-dex-aggregator
npm test
npm run typecheck
npm run build
```

Unit/integration tests use real SAC tokens with protocol fixtures and specifically mocked **sender** authorization. Aggregator-owned nested transfer/approval authorization is enforced, not globally mocked during swaps. Tests cover both modes on all five adapters, custom/self destinations, mixed multihop routes, splits, fees, donations, caps, bad routes, pauses/failures, admin fee auth, protocol tampering, unknown/incompatible pools, unauthorized over-pulls, expiry, overflow, and rollback. Property tests cover fee/gross minimality and rounding.

Fork tests load actual deployed bytecode and storage through the SDK snapshot-source interface. Only local fixture balances are funded; there are no Mainnet writes, signing, or submissions. The first capture discovers necessary ledger keys lazily. A second live capture refreshes the discovered footprint in one batch, which provides a consistent observed state for subsequent offline runs. Capture metadata records its ledger range. Offline tests fail on missing keys instead of inventing state. Fresh captures can fail if a pool is paused, has changed ABI, or no longer has sufficient liquidity.

The verification report records executed checks and resource measurements. Successful local/fork tests are not proof of a funded network transaction or production deployment. Deployment, signing with an actual wallet, classic-account trustlines, network resource settings, and the selected route's current simulation remain separate checks.

## Return value and errors

`SwapResult` returns `amount_in` (original input actually spent after its refund), `amount_out` (net output delivered), `fee` (output token paid to treasury), and `surplus_out` (extra output refunded to sender). Intermediate refunds are visible in the token transfer events. The contract emits `Swapped` and `FeeChanged`.

| Code | Name | Meaning |
| --- | --- | --- |
| 1 | InvalidAmount | Zero/negative amount or invalid hop constraints |
| 2 | InvalidFee | Aggregator fee is outside 0..9999 bps |
| 3 | FeeChanged | Signed rate differs from current configuration |
| 4 | Expired | Ledger time is after the signed deadline |
| 5 | InvalidRoute | Empty, discontinuous, cyclic, or repeated pool route |
| 6 | TooManySteps | More than 8 legs or 4 hops in a leg |
| 8 | InvalidPool | Invalid discovered token list or route token membership |
| 9 | InconsistentTotals | Leg/hop amounts do not cover signed aggregate bounds |
| 10 | MinimumNotMet | Observed output does not cover a hop/leg/total floor |
| 11 | MaximumExceeded | Required input exceeds a signed hop budget |
| 12 | BalanceInvariant | Wrong spend, unexpected balance change, or donation consumed |
| 13 | Overflow | An amount does not fit checked integer arithmetic |
| 14 | InvalidRecipient | Aggregator itself used as sender, recipient, or treasury |

Protocol errors and token/trustline failures also abort the invocation; they are not remapped to these codes.

The captured Phoenix pool `CBENAB...` currently has a bootstrap trading floor. Its quote getter returns a positive output but its swap returns error 337. The collector now reads Phoenix floor/delegation storage, excludes unmet floors, and refuses outputs that rely on assets held by a delegate. The old example is a format example, not a current executable price.

Fork tests raise the SDK's diagnostic authorization-snapshot budget for long call trees. They separately assert actual execution usage below 100 million instructions and 40 MiB; the SDK's diagnostic bookkeeping is not part of on-chain execution. The verification report records the compiled Wasm measurements. These thresholds do not replace network simulation of the selected route.
