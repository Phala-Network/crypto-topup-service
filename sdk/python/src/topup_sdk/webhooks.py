"""Standard Webhooks verification for events signed with the service's settlement key.

Events carry `webhook-id`, `webhook-timestamp`, and `webhook-signature` headers. The signature
is the asymmetric `v1a` scheme: ed25519 over `{id}.{timestamp}.{body}` with the settlement key
pinned from attestation. Receivers deduplicate by `webhook-id`; `deposit.credited` is the
fulfillment event (`topup_sdk.fulfillment`), every other type is informational.
"""

from __future__ import annotations

import base64
import binascii
import json
import time
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

from .errors import SignatureError

DEFAULT_TOLERANCE_SECONDS = 300


@dataclass(frozen=True)
class WebhookEvent:
    """A verified event envelope."""

    id: str
    type: str
    created_at: str
    data: dict[str, Any]


def verify_webhook(
    headers: Mapping[str, str],
    body: bytes,
    public_key: Ed25519PublicKey,
    *,
    tolerance_seconds: int = DEFAULT_TOLERANCE_SECONDS,
    now: int | None = None,
) -> WebhookEvent:
    """Verifies a delivery and returns its parsed envelope, raising `SignatureError` otherwise."""
    webhook_id = verify_webhook_signature(
        headers, body, public_key, tolerance_seconds=tolerance_seconds, now=now
    )
    try:
        envelope = json.loads(body)
        event = WebhookEvent(
            id=str(envelope["event_id"]),
            type=str(envelope["type"]),
            created_at=str(envelope["created_at"]),
            data=dict(envelope["data"]),
        )
    except (ValueError, KeyError, TypeError) as error:
        raise SignatureError("webhook body malformed") from error
    if event.id != webhook_id:
        raise SignatureError("webhook id does not match the envelope")
    return event


def verify_webhook_signature(
    headers: Mapping[str, str],
    body: bytes,
    public_key: Ed25519PublicKey,
    *,
    tolerance_seconds: int = DEFAULT_TOLERANCE_SECONDS,
    now: int | None = None,
) -> str:
    """Checks the Standard Webhooks `v1a` signature and timestamp; returns the `webhook-id`."""
    lowered = {name.lower(): value.strip() for name, value in headers.items()}
    try:
        webhook_id = lowered["webhook-id"]
        timestamp = lowered["webhook-timestamp"]
        signatures = lowered["webhook-signature"]
    except KeyError as error:
        raise SignatureError("webhook headers missing") from error
    if not (timestamp.isascii() and timestamp.isdigit()):
        raise SignatureError("webhook timestamp malformed")
    now = int(time.time()) if now is None else now
    if abs(now - int(timestamp)) > tolerance_seconds:
        raise SignatureError("webhook timestamp outside tolerance")

    content = f"{webhook_id}.{timestamp}.".encode() + body
    if not any(_matches(entry, content, public_key) for entry in signatures.split()):
        raise SignatureError("no valid webhook signature")
    return webhook_id


def sign_webhook(
    private_key: Ed25519PrivateKey, webhook_id: str, timestamp: int, body: bytes
) -> dict[str, str]:
    """Returns Standard Webhooks `v1a` headers, as the service signs a delivery.

    For test senders only: a product never holds the service's key.
    """
    content = f"{webhook_id}.{timestamp}.".encode() + body
    signature = base64.b64encode(private_key.sign(content)).decode("ascii")
    return {
        "webhook-id": webhook_id,
        "webhook-timestamp": str(timestamp),
        "webhook-signature": f"v1a,{signature}",
    }


def _matches(entry: str, content: bytes, public_key: Ed25519PublicKey) -> bool:
    version, _, encoded = entry.partition(",")
    if version != "v1a":
        return False
    try:
        public_key.verify(base64.b64decode(encoded, validate=True), content)
    except (InvalidSignature, binascii.Error, ValueError):
        return False
    return True
