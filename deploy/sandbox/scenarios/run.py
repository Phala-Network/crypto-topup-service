"""Runs the sandbox scenarios against a configured stack.

    uv run --project sdk/python python deploy/sandbox/scenarios/run.py --config FILE [NAME ...]

Exit status is zero only if every selected scenario passed or was explicitly skipped.
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
import time
from collections.abc import Callable
from types import ModuleType

import happy_path
import harness
import late_payment
import overpayment
import phala_cloud_integration as reference
import product_refusal
import restart_mid_flow
import underpayment
import unsupported_asset

SCENARIOS: dict[str, ModuleType] = {
    "happy_path": happy_path,
    "late_payment": late_payment,
    "underpayment": underpayment,
    "overpayment": overpayment,
    "unsupported_asset": unsupported_asset,
    "product_refusal": product_refusal,
    "restart_mid_flow": restart_mid_flow,
}


def main() -> int:
    parser = argparse.ArgumentParser(description="Run sandbox scenarios.")
    parser.add_argument("--config", default=os.environ.get("SANDBOX_CONFIG"))
    parser.add_argument("names", nargs="*", metavar="NAME", help=", ".join(SCENARIOS))
    args = parser.parse_args()
    if not args.config:
        parser.error("--config or SANDBOX_CONFIG is required")
    unknown = sorted(set(args.names) - set(SCENARIOS))
    if unknown:
        parser.error(f"unknown scenarios: {', '.join(unknown)}")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)

    config = reference.SandboxConfig.load(args.config)
    rpc = reference.JsonRpc(config.rpc_url)
    ledger = reference.ProductLedger()
    results: list[tuple[str, str, float, str]] = []
    with config.client() as client:
        key = reference.pin_settlement_key(config, client)
        settlement = harness.ScenarioSettlement(config, ledger, key, rpc)
        with reference.ProductServer(settlement, reference.WebhookReceiver(ledger, key)):
            context = harness.Context(
                config, client, ledger, settlement, reference.Payer(config, rpc)
            )
            for name in args.names or list(SCENARIOS):
                results.append(_run(name, SCENARIOS[name].run, context))

    print("\nscenario            result   seconds  detail")
    for name, result, seconds, detail in results:
        print(f"{name:<19} {result:<8} {seconds:>7.1f}  {detail}")
    failed = [name for name, result, _, _ in results if result == "FAIL"]
    print(f"\n{len(results) - len(failed)} of {len(results)} scenarios passed or skipped")
    return 1 if failed else 0


def _run(
    name: str, scenario: Callable[[harness.Context], None], context: harness.Context
) -> tuple[str, str, float, str]:
    logging.info("=== %s", name)
    started = time.monotonic()
    try:
        scenario(context)
    except harness.ScenarioSkipped as skipped:
        return name, "SKIP", time.monotonic() - started, str(skipped)
    except Exception as error:  # a failed scenario must not stop the others
        logging.exception("scenario %s failed", name)
        return name, "FAIL", time.monotonic() - started, f"{type(error).__name__}: {error}"
    return name, "PASS", time.monotonic() - started, ""


if __name__ == "__main__":
    sys.exit(main())
