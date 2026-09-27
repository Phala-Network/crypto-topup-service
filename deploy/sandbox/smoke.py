"""Sandbox smoke check: the calls a product makes against a Phala Pay service, with their checks.

These are the calls a product such as Phala Cloud makes, and the checks it adds to each:

1. pin the service's settlement key from attestation evidence bound to a fresh nonce;
2. create a quote (the account is created with it); the client recomputes its single-use address
   from the pinned forwarder;
3. list the account's deposits;
4. receive webhooks (Standard Webhooks) and fulfill each `deposit.credited` once.

The handler of step 4 goes behind the product's webhook URL. It stops where the SDK stops: the
product commits the credit, keyed by `credit.fulfillment_key` under a unique index, before
answering `2xx`. deploy/product/reference_product is a complete product that does all of it.

Run steps 1-3 against the sandbox (deploy/sandbox/README.md):

    uv run --locked --project sdk/python python deploy/sandbox/smoke.py \\
        --config sandbox.json
"""

from __future__ import annotations

import argparse
import json
import secrets
import uuid
from collections.abc import Callable, Mapping
from dataclasses import dataclass, fields
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import Quote
from topup_sdk import (
    AttestationError,
    CreditedDeposit,
    RequestSigner,
    TopupClient,
    WebhookEvent,
    load_public_key,
    verify_webhook,
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
    treasury: str
    token_symbol: str

    @classmethod
    def load(cls, path: str | Path) -> Integration:
        values = json.loads(Path(path).read_text(encoding="utf-8"))
        return cls(**{field.name: values[field.name] for field in fields(cls)})

    def client(self) -> TopupClient:
        """A client whose key id, `{product}/v1`, names the product; with the forwarder pinned,
        it recomputes every open quote's address before returning it."""
        signer = RequestSigner.from_seed_file(self.product_keyid, self.product_seed_file)
        return TopupClient(
            self.service_url,
            signer,
            forwarder=(self.factory, self.implementation, self.treasury),
        )


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


# 2-3. Quotes and deposits --------------------------------------------------------------------


def quote(config: Integration, client: TopupClient, account: str, amount_minor: int) -> Quote:
    """Quotes `amount_minor` cents; the account is created with its first quote.

    The client raises `AddressMismatchError` before returning an address it did not derive from
    the pinned forwarder, the product slug, the account, and the quote id.
    """
    return client.create_quote(
        account, amount_minor, chain_id=config.chain_id, asset=config.token_symbol.lower()
    )


# 4. Webhooks and fulfillment ---------------------------------------------------------------------


def receive_webhook(
    settlement_key: Ed25519PublicKey,
    headers: Mapping[str, str],
    body: bytes,
    fulfill: Callable[[CreditedDeposit], None],
) -> WebhookEvent:
    """Verifies a delivery and fulfills it when it is `deposit.credited`.

    `SignatureError` means answer `400`. `fulfill` must credit `credit.amount` cents to
    `credit.account_id` at most once per `credit.fulfillment_key` (the `dep_` id), committing
    before this returns, and treat a repeat as done; answer `2xx` only after it returns. Every
    other event type is informational: notify the user and refresh history.
    """
    event = verify_webhook(headers, body, settlement_key)
    if event.type == "deposit.credited":
        fulfill(CreditedDeposit.from_event(event))
    return event


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
        lock = quote(config, client, account, args.amount_minor)
        print(
            f"quote {lock.id}: pay {lock.amount_atomic} atomic to {lock.address} before "
            f"Unix time {lock.expires_at} for {lock.amount} cents ({lock.payment_uri})"
        )
        deposits = [(item.id, item.status) for item in client.list_deposits(account_id=account)]
        print(f"deposits: {deposits}")
    print("smoke: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
