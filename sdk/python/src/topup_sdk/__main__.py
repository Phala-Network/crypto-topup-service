"""Command-line helpers for integrators.

`keygen` creates an ed25519 seed file, such as an operator's admin key or a test webhook key, and
prints its key id and public key: `public_key` for an RFC 9421 request-signing key, and
`webhook_public_key`, the same key in Standard Webhooks' `whpk_` form, for a webhook key. The seed
file never leaves its machine; `public-key` prints the same for an existing seed file.

`send-test-event` exercises a product's webhook receiver the way `stripe trigger` does: it signs a
synthetic test-mode `deposit.credited` of `--account` with a test seed the receiver's test
instance pins in place of the account's webhook key, delivers it, delivers it again, delivers it
once more with a foreign signature, and once as another account's event signed with the pinned
key. It passes when the first two answers are `2xx` and the others `4xx`; the product then checks
its ledger holds exactly one credit of `--amount` cents for `--client-reference-id`.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import sys
import time
from pathlib import Path
from typing import Any

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from .addresses import deposit_id
from .fulfillment import CREDITED_EVENT, credited_event_id
from .signing import RequestSigner
from .webhooks import WEBHOOK_PUBLIC_KEY_PREFIX, sign_webhook


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="topup-sdk")
    commands = parser.add_subparsers(dest="command", required=True)
    keygen = commands.add_parser("keygen", help="create an ed25519 signing key")
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
        "--seed-file",
        required=True,
        type=Path,
        help="test seed the receiver pins as its webhook key",
    )
    test.add_argument("--account", required=True, help="the receiver's account, acct_…")
    test.add_argument(
        "--client-reference-id", required=True, help="a test customer of the receiver"
    )
    test.add_argument("--amount", type=int, default=100, help="credit in cents")
    args = parser.parse_args(argv)

    if args.command == "send-test-event":
        seed = bytes.fromhex(args.seed_file.read_text(encoding="ascii").strip())
        report = send_test_event(
            args.url,
            Ed25519PrivateKey.from_private_bytes(seed),
            account=args.account,
            client_reference_id=args.client_reference_id,
            amount=args.amount,
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
    public_key = signer.public_key_base64()
    print(
        json.dumps(
            {
                "keyid": signer.keyid,
                "public_key": public_key,
                "webhook_public_key": WEBHOOK_PUBLIC_KEY_PREFIX + public_key,
            }
        )
    )
    return 0


def send_test_event(
    url: str,
    key: Ed25519PrivateKey,
    *,
    account: str,
    client_reference_id: str,
    amount: int,
    transport: httpx.BaseTransport | None = None,
) -> dict[str, Any]:
    """Delivers one synthetic test-mode `deposit.credited` of `account` four times and reports
    the answers."""
    tx_hash = "0x" + secrets.token_hex(32)
    deposit = deposit_id(31337, tx_hash, 0)
    event_id = credited_event_id(deposit)
    now = int(time.time())
    envelope = {
        "id": event_id,
        "object": "event",
        "account": account,
        "livemode": False,
        "type": CREDITED_EVENT,
        "created": now,
        "actor": "system",
        "request": None,
        "data": {
            "object": {
                "id": deposit,
                "object": "deposit",
                "livemode": False,
                "client_reference_id": client_reference_id,
                "quote": None,
                "deposit_address": None,
                "status": "credited",
                "final": False,
                "final_at": None,
                "swept": False,
                "rejection_reason": None,
                "chain_id": 31337,
                "asset": "test",
                "asset_contract": "0x" + "00" * 20,
                "amount_atomic": str(amount),
                "amount": amount,
                "currency": "usd",
                "exchange_rate": "0.10000000",
                "price_source": "spot",
                "valued_at": now,
                "address": "0x" + "00" * 20,
                "from_address": "0x" + "00" * 20,
                "tx_hash": tx_hash,
                "receipt_log_index": 0,
                "revision": 0,
                "log_index": 0,
                "block_number": 1,
                "block_hash": "0x" + "00" * 32,
                "block_time": now,
                "amount_refunded_atomic": "0",
                "refunded": False,
                "amount_refunded": 0,
                "amount_reversed": 0,
                "replaces": None,
                "replaced_by": None,
                "created": now,
                "metadata": {},
            }
        },
    }
    body = json.dumps(envelope, separators=(",", ":")).encode()
    other_account = json.dumps(
        {**envelope, "account": "acct_" + "0" * 32}, separators=(",", ":")
    ).encode()
    cases = [("first", key, body, True), ("duplicate", key, body, True)]
    cases.append(("foreign_signature", Ed25519PrivateKey.generate(), body, False))
    cases.append(("other_account", key, other_account, False))
    results = []
    with httpx.Client(timeout=20, follow_redirects=False, transport=transport) as client:
        for case, signing_key, content, accept in cases:
            headers = sign_webhook(signing_key, event_id, int(time.time()), content)
            headers["content-type"] = "application/json"
            try:
                status: int | None = client.post(url, content=content, headers=headers).status_code
            except httpx.HTTPError:
                status = None
            ok = status is not None and (200 <= status < 300 if accept else 400 <= status < 500)
            results.append({"case": case, "status": status, "ok": ok})
    return {
        "deposit_id": deposit,
        "event_id": event_id,
        "results": results,
        "passed": all(result["ok"] for result in results),
        "then_check": f"exactly one credit of {amount} cents for {client_reference_id}",
    }


if __name__ == "__main__":
    raise SystemExit(main())
