# Lumix spot aggregator

One standalone Soroban contract for atomic exact-input and exact-output swaps, splits, mixed multihop routes, optional destinations, and output-token fees defaulting to zero.

[ABI and twelve examples](../../docs/spot-aggregator.md) · [Executed checks](../../docs/spot-aggregator-verification.md)

```sh
cargo test
stellar contract build
cargo test --features wasm-tests --lib
# Offline deployed-protocol fork tests, using the included Mainnet capture:
cargo test --features wasm-tests --test mainnet_fork -- --ignored --nocapture
```

SDK 29.0.1 lives in this separate workspace. The existing contracts retain SDK 27. No pool registration is required. Each signed hop supplies its pool address and protocol; token order is discovered from the pool, and every adapter calls the pool directly. Phoenix/Sushi fixed-output delivery uses bounded input with surplus refunds. No deployment or funded network swap has been performed.
