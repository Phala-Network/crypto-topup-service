"""Integrating a product with the crypto top-up service through the Python SDK.

These are the calls a product such as Phala Cloud makes, and the checks it adds to each:

1. pin the service's settlement key from attestation evidence bound to a fresh nonce;
2. register an account and recompute its persistent deposit address before showing it;
3. create a quote-first rate lock and recompute its single-use address before showing it;
4. list the account's deposits;
5. verify inbound settlement requests (RFC 9421) and webhook deliveries (Standard Webhooks).

The two handlers of step 5 go behind the product's settlement and webhook URLs. The settlement
handler stops where the SDK stops: the product still enforces its caps, verifies the cited log
with its own RPC, and commits the credit atomically before answering (docs/architecture.md
section 11). deploy/product/reference_product is a complete product that does all of it, held to
the contract by the conformance suite (docs/conformance.md).

Run steps 1-4 against the sandbox (deploy/sandbox/README.md):

    uv run --locked --project sdk/python python sdk/examples/phala_cloud_integration.py \\
        --config sandbox.json
"""

from __future__ import annotations

import argparse
import json
import secrets
import uuid
from collections.abc import Mapping
from dataclasses import dataclass, fields
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import RateLockResponse
from topup_sdk import (
    AttestationError,
    RequestSigner,
    TopupClient,
    WebhookEvent,
    load_public_key,
    verify_request,
    verify_webhook,
)
from topup_sdk.addresses import (
    deposit_id,
    forwarder_address,
    lock_salt,
    persistent_salt,
    same_address,
)

SETTLEMENT_KEYID = "settlement/v1"


@dataclass(frozen=True)
class Integration:
    """The product's settings; the sandbox config (deploy/sandbox/README.md) carries them all."""

    service_url: str
    product_slug: str
    product_keyid: str
    product_seed_file: str
    chain_id: int
    factory: str
    implementation: str

    @classmethod
    def load(cls, path: str | Path) -> Integration:
        values = json.loads(Path(path).read_text(encoding="utf-8"))
        return cls(**{field.name: values[field.name] for field in fields(cls)})

    def client(self) -> TopupClient:
        signer = RequestSigner.from_seed_file(self.product_keyid, self.product_seed_file)
        return TopupClient(self.service_url, self.product_slug, signer)


# 1. The settlement key ---------------------------------------------------------------------------


def pin_settlement_key(client: TopupClient) -> Ed25519PublicKey:
    """Returns the settlement key from attestation evidence bound to a fresh nonce.

    `TopupClient.attestation` checks that the report data binds the nonce, the key, and the
    flusher operators. In production, also verify the TDX quote with the dstack verifier
    (deploy/dstack-verifier.sh), then pin `(keyid, public key)` in configuration.
    """
    evidence = client.attestation(secrets.token_bytes(32))
    if evidence.keyid != SETTLEMENT_KEYID:
        raise AttestationError("attestation names an unexpected settlement key id")
    return load_public_key(evidence.settlement_pubkey)


# 2-4. Accounts, addresses, quotes, and deposits --------------------------------------------------


def register(config: Integration, client: TopupClient, account: str) -> str:
    """Registers an account and returns its persistent address, recomputed from its salt."""
    client.register_account(account)
    address = client.create_deposit_address(account)
    salt = persistent_salt(config.product_slug, account, address.salt_inputs.version)
    expected = forwarder_address(config.factory, config.implementation, salt)
    if not same_address(expected, address.address) or address.chain_id != config.chain_id:
        raise RuntimeError("the service returned an address the product cannot recompute")
    return address.address


def quote(
    config: Integration, client: TopupClient, account: str, lock_ref: str, amount_minor: int
) -> RateLockResponse:
    """Creates a quote-first lock whose single-use address the product computed itself.

    Record `expected` as the account's before the request, so a crash in between never leaves a
    paid address the product does not recognise.
    """
    salt = lock_salt(config.product_slug, account, lock_ref)
    expected = forwarder_address(config.factory, config.implementation, salt)
    lock = client.create_rate_lock(account, lock_ref, amount_minor=amount_minor)
    if not same_address(expected, lock.address):
        raise RuntimeError("the rate-lock address does not match the product's computation")
    return lock


# 5. Inbound settlement requests and webhooks -----------------------------------------------------


def verify_settlement(
    public_url: str,
    settlement_key: Ed25519PublicKey,
    path: str,
    headers: Mapping[str, str],
    body: bytes,
) -> dict[str, Any]:
    """Returns a `POST {public_url}/settlements` payload whose signature and key check out.

    Raises `SignatureError` (answer `401`) unless the request is signed by the pinned key over
    the method, target URI, body digest, and idempotency key, and `ValueError` (answer `422`)
    unless the key names the payload and equals `deposit:` plus the deposit id recomputed from
    the cited chain event. The target URI comes from the product's own public URL, never from
    the `Host` header.
    """
    public = urlsplit(public_url)
    verified = verify_request(
        method="POST",
        target_uri=f"{public.scheme}://{public.netloc}{path}",
        headers=headers,
        body=body,
        public_key=settlement_key,
        keyid=SETTLEMENT_KEYID,
        require_idempotency_key=True,
    )
    payload = json.loads(body)
    if not isinstance(payload, dict) or payload.get("idempotency_key") != verified.idempotency_key:
        raise ValueError("the idempotency key does not name this payload")
    try:
        evidence = payload["evidence"]
        expected = deposit_id(evidence["chain_id"], evidence["tx_hash"], evidence["log_index"])
    except (KeyError, TypeError) as error:
        raise ValueError("the payload cites no chain event") from error
    if verified.idempotency_key != f"deposit:{expected}":
        raise ValueError("the idempotency key is not the cited event's deposit id")
    return payload


def receive_webhook(
    settlement_key: Ed25519PublicKey, headers: Mapping[str, str], body: bytes
) -> WebhookEvent:
    """Returns a verified event; `SignatureError` means answer `401`.

    Store events once by `event.id`, which stays the same across redeliveries. Events never move
    balances: settlement requests do. Use them to notify the user and refresh history.
    """
    return verify_webhook(headers, body, settlement_key)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", required=True, help="the sandbox config JSON")
    parser.add_argument("--amount-minor", type=int, default=2500, help="quote amount in cents")
    args = parser.parse_args()
    config = Integration.load(args.config)
    with config.client() as client:
        pin_settlement_key(client)
        print("pinned the settlement key from attestation")
        account = f"example-{uuid.uuid4().hex[:12]}"
        print(f"registered {account}; persistent address {register(config, client, account)}")
        lock = quote(
            config, client, account, f"checkout-{uuid.uuid4().hex[:12]}", args.amount_minor
        )
        print(
            f"quote: pay {lock.amount_atomic} atomic to {lock.address} before "
            f"{lock.expires_at.isoformat()} for {lock.credit_minor} minor ({lock.eip681_uri})"
        )
        deposits = [(str(item.id), item.state) for item in client.list_deposits(account)]
        print(f"deposits: {deposits}")
    print("phala_cloud_integration: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
