# Fuzzing Lumix

Three executable libFuzzer targets exercise real contract calls:

- `oracle_history`: out-of-order publications and conflicting retries against an independent map of immutable observations.
- `perp_lifecycle`: request escrow, missing prices, execution, cancellation, liquidation with a pending close, and token conservation across bounded action sequences.
- `option_lifecycle`: call/put order escrow, separate request timestamps, expiry-only settlement, replay rejection, and independent payoff calculations.

The same scenarios run as `proptest` cases in `cargo test --workspace`. Failures shrink to reproducible inputs. Financial math also has property tests in `lumix-core`.

```bash
cargo install cargo-fuzz --locked
cargo +nightly fuzz run oracle_history -- -max_total_time=300 -max_len=256
cargo +nightly fuzz run perp_lifecycle -- -max_total_time=300 -max_len=32
cargo +nightly fuzz run option_lifecycle -- -max_total_time=300 -max_len=12
```

On this macOS ARM host, the installed nightly's thread sanitizer has a standard-library ABI mismatch. Coverage-guided fuzzing works with sanitizers disabled and dynamic lookup for the unused native contract dylibs:

```bash
export RUSTFLAGS='-C link-arg=-Wl,-undefined,dynamic_lookup'
cargo +nightly fuzz run --sanitizer=none oracle_history -- -max_total_time=60 -max_len=256
cargo +nightly fuzz run --sanitizer=none perp_lifecycle -- -max_total_time=60 -max_len=32
cargo +nightly fuzz run --sanitizer=none option_lifecycle -- -max_total_time=60 -max_len=12
```

The executable still uses libFuzzer coverage instrumentation and invariant assertions. This mode does not provide address/thread sanitizer checks. Preserve any crashing input under `fuzz/artifacts` and convert it into a regression test. Fuzzer corpora and build output are ignored by Git.

The test approach follows Stellar's [fuzzing guide](https://developers.stellar.org/docs/build/guides/testing/fuzzing). Focused mutations run separately with `python3 scripts/test-mutations.py`; their build artifacts are isolated from ordinary tests.
