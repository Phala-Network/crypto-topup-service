"""Command-line helpers for integrators.

`keygen` creates a product signing key for credential issuance: the seed file stays with the
integrator, and only the printed public key and key id are sent to the service operator.

`send-test-event` exercises a product's webhook receiver the way `stripe trigger` does: it signs a
synthetic `deposit.credited` with a test seed the receiver's test instance pins in place of the
service key, delivers it, delivers it again, and delivers it once more with a foreign signature.
It passes when the first two answers are `2xx` and the third is `4xx`; the product then checks
its ledger holds exactly one credit of `--amount-minor` for `--external-id`.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import sys
import time
import uuid
from pathlib import Path
from typing import Any

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from .addresses import deposit_id
from .fulfillment import CREDITED_EVENT, credited_event_id
from .signing import RequestSigner
from .webhooks import sign_webhook


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="topup-sdk")
    commands = parser.add_subparsers(dest="command", required=True)
    keygen = commands.add_parser("keygen", help="create an ed25519 product signing key")
    keygen.add_argument("--keyid", required=True, help="key identifier, for example acme/v1")
    keygen.add_argument("--seed-out", required=True, type=Path, help="new file for the seed")
    public = commands.add_parser("public-key", help="print the public key of a seed file")
    public.add_argument("--keyid", required=True)
    public.add_argument("--seed-file", required=True, type=Path)
    test = commands.add_parser(
        "send-test-event", help="deliver signed test deposit.credited events to a receiver"
    )
    test.add_argument("--url", required=True, help="the receiver's webhook URL")
    test.add_argument(
        "--seed-file", required=True, type=Path, help="test seed the receiver pins as service key"
    )
    test.add_argument("--external-id", required=True, help="a test account of the receiver")
    test.add_argument("--amount-minor", type=int, default=100)
    test.add_argument("--product-id", type=uuid.UUID, default=uuid.UUID(int=0))
    args = parser.parse_args(argv)

    if args.command == "send-test-event":
        seed = bytes.fromhex(args.seed_file.read_text(encoding="ascii").strip())
        report = send_test_event(
            args.url,
            Ed25519PrivateKey.from_private_bytes(seed),
            external_id=args.external_id,
            amount_minor=args.amount_minor,
            product_id=args.product_id,
        )
        print(json.dumps(report, indent=2))
        return 0 if report["passed"] else 1
    if args.command == "keygen":
        seed = secrets.token_bytes(32)
        try:
            descriptor = os.open(args.seed_out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            print(f"refusing to overwrite {args.seed_out}", file=sys.stderr)
            return 1
        with os.fdopen(descriptor, "w", encoding="ascii") as handle:
            handle.write(seed.hex() + "\n")
        signer = RequestSigner.from_seed(args.keyid, seed)
    else:
        signer = RequestSigner.from_seed_file(args.keyid, args.seed_file)
    print(json.dumps({"keyid": signer.keyid, "public_key": signer.public_key_base64()}))
    return 0


def send_test_event(
    url: str,
    key: Ed25519PrivateKey,
    *,
    external_id: str,
    amount_minor: int,
    product_id: uuid.UUID,
    transport: httpx.BaseTransport | None = None,
) -> dict[str, Any]:
    """Delivers one synthetic `deposit.credited` three times and reports the answers."""
    tx_hash = "0x" + secrets.token_hex(32)
    deposit = deposit_id(31337, tx_hash, 0)
    event_id = str(credited_event_id(deposit))
    body = json.dumps(
        {
            "event_id": event_id,
            "type": CREDITED_EVENT,
            "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "data": {
                "product_id": str(product_id),
                "external_id": external_id,
                "deposit_id": str(deposit),
                "state": "credited",
                "unit": "USD",
                "amount_minor": str(amount_minor),
                "price_source": "spot",
                "price_scaled": "10000000",
                "price_scale": 8,
                "valuation_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "product_lock_ref": None,
                "address": "0x" + "00" * 20,
                "route": "test",
                "route_version": 1,
                "chain_id": 31337,
                "asset_contract": "0x" + "00" * 20,
                "tx_hash": tx_hash,
                "log_index": 0,
                "amount_atomic": str(amount_minor),
            },
        },
        separators=(",", ":"),
    ).encode()
    cases = [("first", key, True), ("duplicate", key, True)]
    cases.append(("foreign_signature", Ed25519PrivateKey.generate(), False))
    results = []
    with httpx.Client(timeout=20, follow_redirects=False, transport=transport) as client:
        for case, signing_key, accept in cases:
            headers = sign_webhook(signing_key, event_id, int(time.time()), body)
            headers["content-type"] = "application/json"
            try:
                status: int | None = client.post(url, content=body, headers=headers).status_code
            except httpx.HTTPError:
                status = None
            ok = status is not None and (200 <= status < 300 if accept else 400 <= status < 500)
            results.append({"case": case, "status": status, "ok": ok})
    return {
        "deposit_id": str(deposit),
        "event_id": event_id,
        "results": results,
        "passed": all(result["ok"] for result in results),
        "then_check": f"exactly one credit of {amount_minor} for {external_id}",
    }


if __name__ == "__main__":
    raise SystemExit(main())
