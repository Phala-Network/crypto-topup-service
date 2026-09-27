"""The staging reference product for the crypto top-up service.

`serve` runs the product service (reference_product.server); `deposit` drives one deposit through
a running product (reference_product.driver); with no mode, both run in one process
(deploy/sandbox/run-local.sh). See deploy/README.md, "Staging reference product":

    PYTHONPATH=deploy/product uv run --locked --project sdk/python \\
        python -m reference_product [MODE] --config FILE
"""

from __future__ import annotations

import argparse
import logging
import os
import secrets
from dataclasses import replace

from topup_sdk import RequestSigner

from .config import DRIVER_KEYID, EVM_ADDRESS, ProductConfig
from .driver import run_deposit
from .server import product_service, serve


def run_local(config: ProductConfig) -> None:
    """Serves the product and drives one deposit through it, in one process."""
    driver = RequestSigner.from_seed(DRIVER_KEYID, secrets.token_bytes(32))
    config = replace(config, driver_public_key=driver.public_key_base64())
    with product_service(config):
        run_deposit(config, driver, amount_minor=2500, timeout=420)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "mode",
        nargs="?",
        choices=["serve", "deposit"],
        help="serve the product, or drive one deposit through a running product; "
        "without a mode, both in one process",
    )
    parser.add_argument("--config", default=os.environ.get("SANDBOX_CONFIG"))
    deposit = parser.add_argument_group("deposit")
    deposit.add_argument("--driver-seed-file", help="seed file of the driver key (driver/v1)")
    deposit.add_argument("--amount-minor", type=int, default=2500, help="quote amount in cents")
    deposit.add_argument(
        "--min-atomic",
        type=int,
        default=0,
        help="refuse to pay fewer atomic units (the route's min_flush_atomic)",
    )
    deposit.add_argument(
        "--until",
        choices=["credited", "swept", "rejected", "refunded"],
        default="credited",
        help="the outcome to wait for; refunded requests a refund of a rejected deposit",
    )
    deposit.add_argument("--timeout", type=float, default=1800, help="seconds to wait")
    deposit.add_argument(
        "--pay-bps",
        type=int,
        default=10_000,
        help="pay this fraction of the quoted amount, in basis points (credited at spot)",
    )
    deposit.add_argument(
        "--pay-after-expiry",
        action="store_true",
        help="pay the quoted amount after the quote window (credited at spot)",
    )
    deposit.add_argument("--token", help="pay with this token instead of the route's")
    deposit.add_argument("--refund-to", help="refund destination for --until refunded")
    args = parser.parse_args()
    if not args.config:
        parser.error("--config or SANDBOX_CONFIG is required")
    if args.mode == "deposit" and not args.driver_seed_file:
        parser.error("deposit needs --driver-seed-file")
    if args.pay_bps <= 0:
        parser.error("--pay-bps must be positive")
    if (args.until == "refunded") != (args.refund_to is not None):
        parser.error("--refund-to goes with --until refunded, and only with it")
    if args.refund_to is not None and not EVM_ADDRESS.fullmatch(args.refund_to):
        parser.error("--refund-to must be a 0x-prefixed 20-byte address")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    config = ProductConfig.load(args.config)
    if args.mode == "serve":
        serve(config)
        return 0
    if args.mode == "deposit":
        run_deposit(
            config,
            RequestSigner.from_seed_file(DRIVER_KEYID, args.driver_seed_file),
            amount_minor=args.amount_minor,
            min_atomic=args.min_atomic,
            until=args.until,
            timeout=args.timeout,
            pay_bps=args.pay_bps,
            pay_after_expiry=args.pay_after_expiry,
            token=args.token,
            refund_to=args.refund_to,
        )
    else:
        run_local(config)
    print("reference_product: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
