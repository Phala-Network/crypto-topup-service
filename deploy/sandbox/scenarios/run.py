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
import product_refusal
import restart_mid_flow
import underpayment
import unsupported_asset
from reference_product.config import ProductConfig
from reference_product.driver import Payer
from reference_product.ledger import ProductLedger
from reference_product.server import ProductServer, pin_settlement_key
from reference_product.settlement import JsonRpc, WebhookReceiver

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
    parser.add_argument(
        "--skip", action="append", default=[], metavar="NAME", help="scenario to leave out"
    )
    args = parser.parse_args()
    if not args.config:
        parser.error("--config or SANDBOX_CONFIG is required")
    unknown = sorted((set(args.names) | set(args.skip)) - set(SCENARIOS))
    if unknown:
        parser.error(f"unknown scenarios: {', '.join(unknown)}")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)

    config = ProductConfig.load(args.config)
    ledger = ProductLedger()
    results: list[tuple[str, str, float, str]] = []
    with config.client() as client:
        key = pin_settlement_key(config)
        settlement = harness.ScenarioSettlement(config, ledger, key, JsonRpc(config.rpc_url))
        with ProductServer(settlement, WebhookReceiver(ledger, key)):
            context = harness.Context(config, client, ledger, settlement, Payer(config))
            for name in [name for name in args.names or SCENARIOS if name not in args.skip]:
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
