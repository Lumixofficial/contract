#!/usr/bin/env python3
"""Focused mutation checks for the request/oracle safety boundaries.

Copies source to a temporary workspace. Never modifies the working checkout.
A mutation only counts as caught when the selected test actually runs and fails.
"""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
# path, original, replacement, package, test filter
MUTATIONS = [
    ("contracts/lumix-oracle/src/lib.rs",
     "if stored_price(&env, &asset, timestamp).is_some() {",
     "if false {", "lumix-oracle", "first_write_wins_for_asset_and_timestamp"),
    ("contracts/lumix-oracle/src/lib.rs",
     "if timestamp > env.ledger().timestamp() {", "if false {",
     "lumix-oracle", "future_missing_invalid_and_paused"),
    ("contracts/lumix-oracle/src/lib.rs",
     "if fetched.timestamp != timestamp {", "if false {",
     "lumix-oracle", "source_resolves_exact_timestamp"),
    ("contracts/lumix-exchange/src/lib.rs",
     "&o.isolated,\n                &o.price_timestamp,", "&o.isolated,\n                &env.ledger().timestamp(),",
     "lumix-test-suites", "delayed_open_and_close_use_request_prices"),
    ("contracts/lumix-exchange/src/lib.rs",
     "if is_increase(o.order_type) && o.collateral_amount > 0 {", "if false {",
     "lumix-test-suites", "cancellation_refunds_all_escrow"),
    ("contracts/lumix-exchange/src/lib.rs",
     "keeper.require_auth();", "// mutation: missing keeper signature",
     "lumix-test-suites", "execution_needs_only_keeper_auth"),
    ("contracts/lumix-options/src/lib.rs",
     "if env.ledger().timestamp() < position.expiry {", "if false {",
     "lumix-test-suites", "option_orders_validate_both_timestamps"),
    ("contracts/lumix-options/src/lib.rs",
     "require_settler(&env, &caller, &c);", "// mutation: missing matcher role check",
     "lumix-test-suites", "option_orders_validate_both_timestamps"),
    ("contracts/lumix-market/src/lib.rs",
     "require_keeper(&env, &c, &liquidator);", "// mutation: missing liquidation role check",
     "lumix-test-suites", "liquidation_is_separate_from_pending_close"),
    ("contracts/lumix-exchange/src/lib.rs",
     "if info.market != *market {", "if false {",
     "lumix-test-suites", "unregistered_market_cannot_receive_escrow"),
]


def main():
    output = ROOT / "mutants.out-focused"
    output.mkdir(exist_ok=True)
    env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target" / "mutations"))
    failures = []
    with tempfile.TemporaryDirectory(prefix="lumix-mutations-") as temp:
        dest = Path(temp)
        for name in ["contracts", "crates", "test-suites"]:
            shutil.copytree(ROOT / name, dest / name,
                            ignore=shutil.ignore_patterns("test_snapshots", "proptest-regressions"))
        shutil.copy2(ROOT / "Cargo.toml", dest / "Cargo.toml")
        if (ROOT / "Cargo.lock").exists():
            shutil.copy2(ROOT / "Cargo.lock", dest / "Cargo.lock")
        for index, (file, before, after, package, test) in enumerate(MUTATIONS, 1):
            path = dest / file
            source = path.read_text()
            if before not in source:
                raise RuntimeError(f"Mutation no longer matches source: {file}: {before}")
            path.write_text(source.replace(before, after, 1))
            try:
                result = subprocess.run(
                    ["cargo", "test", "--manifest-path", str(dest / "Cargo.toml"),
                     "-p", package, test, "--", "--nocapture"],
                    env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    text=True, timeout=600)
            finally:
                path.write_text(source)
            (output / f"{index:02}-{test}.log").write_text(result.stdout)
            caught = result.returncode != 0 and "test result: FAILED" in result.stdout
            print(f"{'CAUGHT' if caught else 'NOT CAUGHT'} {index}/{len(MUTATIONS)} {test}", flush=True)
            if not caught:
                failures.append(test)
    if failures:
        raise SystemExit(f"Mutation checks failed: {', '.join(failures)}")
    print(f"All {len(MUTATIONS)} targeted mutations caught.")


if __name__ == "__main__":
    main()
