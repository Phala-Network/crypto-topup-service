"""The staging reference product for the Phala Pay service.

`serve` runs the product service (reference_product.server); `deposit` drives one deposit through
a running product (reference_product.driver); with no mode, both run in one process
(deploy/sandbox/run-local.sh). `export-restore-records` prints the records a service restore asks
the merchant for (reference_product.restore_records). See deploy/phala.md, "Staging reference
product":

    PYTHONPATH=deploy/product uv run --locked --project sdk/python \\
        python -m reference_product [MODE] --config FILE
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import secrets
import sys
from dataclasses import replace

from topup_sdk import RequestSigner

from .config import DRIVER_KEYID, EVM_ADDRESS, ProductConfig
from .driver import run_deposit
from .ledger import ProductLedger
from .restore_records import export_restore_records
from .server import product_service, serve


def run_local(config: ProductConfig) -> None:
    """Serves the product and drives one deposit through it, in one process."""
    driver = RequestSigner.from_seed(DRIVER_KEYID, secrets.token_bytes(32))
    config = replace(config, driver_public_key=driver.public_key_base64())
    with product_service(config):
        run_deposit(config, driver, amount_minor=2500, timeout=420)


def write_records(text: str, output: str | None) -> None:
    """Writes the records, which hold client secrets, to stdout or to a new owner-only file."""
    if output is None:
        sys.stdout.write(text)
        return
    with os.fdopen(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
        file.write(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "mode",
        nargs="?",
        choices=["serve", "deposit", "export-restore-records"],
        help="serve the product, drive one deposit through a running product, or export its "
        "restore records; without a mode, serve and deposit in one process",
    )
    parser.add_argument("--config", default=os.environ.get("SANDBOX_CONFIG"))
    export = parser.add_argument_group("export-restore-records")
    export.add_argument(
        "--since",
        type=int,
        help="only records the service created from this Unix time on (the restore point)",
    )
    export.add_argument(
        "--output",
        help="write the records to this new file, readable by its owner only (default: stdout)",
    )
    deposit = parser.add_argument_group("deposit")
    deposit.add_argument("--driver-seed-file", help="seed file of the driver key (driver/v1)")
    deposit.add_argument("--amount-minor", type=int, default=2500, help="quote amount in cents")
    deposit.add_argument(
        "--min-atomic",
        type=int,
        default=0,
        help="refuse to pay fewer atomic units",
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
    deposit.add_argument("--token", help="pay with this token instead of the chain's test token")
    deposit.add_argument(
        "--chain-id",
        type=int,
        help="pay on this configured chain (default: the config's first chain)",
    )
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
    if args.chain_id is not None and args.chain_id not in config.treasuries():
        parser.error(f"--chain-id {args.chain_id} is not one of the config's chains")
    if args.mode == "serve":
        serve(config)
        return 0
    if args.mode == "export-restore-records":
        if not os.path.isfile(config.ledger_path):
            parser.error(f"the config's ledger_path {config.ledger_path!r} is not a ledger file")
        records = export_restore_records(
            config.account, ProductLedger(config.ledger_path), since=args.since
        )
        write_records(json.dumps(records, indent=2) + "\n", args.output)
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
            chain_id=args.chain_id,
        )
    else:
        run_local(config)
    print("reference_product: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
