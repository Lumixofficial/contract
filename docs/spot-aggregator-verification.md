# Spot aggregator verification

Checked 2026-10-23T09:42:51.741441+00:00. Changes are uncommitted.

The current ABI has no pool registry. Each hop supplies its pool address and protocol. All five adapters read pool metadata and execute directly against the pool. The constructor still takes admin, treasury, and optional fee basis points.

- 19 native and 19 compiled-Wasm contract tests passed. Coverage includes both swap modes on all adapters, strict sender authorization, real SAC transfers, token membership, incompatible protocols, protocol signature tampering, malicious over-pulls, fees, refunds, donation preservation, rollback, and Soroswap invariant/rounding properties.
- Three native and three compiled-Wasm fork tests passed offline against freshly captured Mainnet bytecode/state. They cover 28 protocol/model/mode/direction cases, mixed four-hop splits in both modes, late failure rollback, and Phoenix bootstrap-floor rejection.
- 282 quote worker tests passed, including the real RabbitMQ integration suite and contract ABI encoding. Quote type checking and build passed.
- 59 API spot/RabbitMQ tests passed. API type checking and build passed.
- 10 interface quote/envelope checks passed, including protocol mismatch and signature-bound payload checks. Interface type checking passed.
- Clippy passed with warnings denied.

The optimized Wasm is 19754 bytes, SHA-256 `b0266a69823eb15d4fab29896d56bcf6d53a5336c7d67602d8a92040bbb460d4`. SDK 29.0.1 remains isolated from the existing SDK 27 workspace.

The captured footprint has 117 keys at ledger 65092474 on protocol 29. The footprint was discovered through read-only RPC and then refreshed in a single batch before the final offline runs. Pool bytecode/storage and SAC code are real. User balances and the aggregator deployment are local fixtures.

Maximum measured Wasm execution across single-pool cases: 27,327,651 instructions and 4,725,584 bytes. Split measurements and every case are in [the JSON report](spot-aggregator-verification.json). The SDK diagnostic authorization-snapshot budget is raised separately; execution is asserted below 100 million instructions and 40 MiB.

No deployment, wallet signing, or funded network swap was performed. No browser test was run for this ABI change. Pool selection is caller-controlled; the protocol tag selects an adapter and does not attest official protocol membership. Phoenix/Sushi fixed-output delivery retains bounded-input swaps with surplus refunds. See [the ABI and examples](spot-aggregator.md).
