"""Standard Webhooks verification for events signed with an account's webhook key.

Events carry `webhook-id`, `webhook-timestamp`, and `webhook-signature` headers. The signature
is the asymmetric `v1a` scheme: ed25519 over `{id}.{timestamp}.{body}` with the account's key in
the event's mode (design D11), pinned from attestation; during a key rotation a delivery carries
one signature per key, and any pinned key may verify it. The body is Stripe's Event object,
`{id, object: "event", account, livemode, type, created, data: {object}}`, where `data.object` is
the object the event is about. Receivers deduplicate by `webhook-id`, the event's `evt_` id;
`deposit.credited` is the fulfillment event (`topup_sdk.fulfillment`).
"""

from __future__ import annotations

import base64
import binascii
import json
import time
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

from .errors import SignatureError

DEFAULT_TOLERANCE_SECONDS = 300

PublicKeys = Ed25519PublicKey | Sequence[Ed25519PublicKey]
"""One pinned webhook key, or several while a rotation overlaps."""


@dataclass(frozen=True)
class WebhookEvent:
    """A verified event of one account and mode: its `evt_` id, type, creation time in Unix
    seconds, and `data`, whose `object` is the object the event is about."""

    id: str
    account: str
    livemode: bool
    type: str
    created: int
    data: dict[str, Any]

    @property
    def object(self) -> dict[str, Any] | None:
        """The object the event is about."""
        value = self.data.get("object")
        return value if isinstance(value, dict) else None


def verify_webhook(
    headers: Mapping[str, str],
    body: bytes,
    public_keys: PublicKeys,
    *,
    expected_account: str,
    expected_livemode: bool,
    tolerance_seconds: int = DEFAULT_TOLERANCE_SECONDS,
    now: int | None = None,
) -> WebhookEvent:
    """Verifies a delivery and returns its parsed envelope, raising `SignatureError` otherwise.

    Fails closed unless a signature verifies with one of `public_keys`, the account's keys
    pinned from attestation, and the event's `account` and `livemode` are `expected_account` and
    `expected_livemode`: a key is per account and mode, and this check also refuses an event of
    another account or mode that verified with a key pinned by mistake.
    """
    webhook_id = verify_webhook_signature(
        headers, body, public_keys, tolerance_seconds=tolerance_seconds, now=now
    )
    try:
        envelope = json.loads(body)
        if (
            envelope["object"] != "event"
            or type(envelope["created"]) is not int
            or not isinstance(envelope["account"], str)
            or type(envelope["livemode"]) is not bool
        ):
            raise ValueError("not an event object")
        event = WebhookEvent(
            id=str(envelope["id"]),
            account=envelope["account"],
            livemode=envelope["livemode"],
            type=str(envelope["type"]),
            created=envelope["created"],
            data=dict(envelope["data"]),
        )
    except (ValueError, KeyError, TypeError) as error:
        raise SignatureError("webhook body malformed") from error
    if event.id != webhook_id:
        raise SignatureError("webhook id does not match the envelope")
    if event.account != expected_account:
        raise SignatureError("webhook event is for another account")
    if event.livemode != expected_livemode:
        raise SignatureError("webhook event is for the other mode")
    return event


def verify_webhook_signature(
    headers: Mapping[str, str],
    body: bytes,
    public_keys: PublicKeys,
    *,
    tolerance_seconds: int = DEFAULT_TOLERANCE_SECONDS,
    now: int | None = None,
) -> str:
    """Checks that a Standard Webhooks `v1a` signature verifies with one of `public_keys` and the
    timestamp is within tolerance; returns the `webhook-id`."""
    keys = [public_keys] if isinstance(public_keys, Ed25519PublicKey) else list(public_keys)
    if not keys:
        raise SignatureError("no webhook public key pinned")
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
    entries = signatures.split()
    if not any(_matches(entry, content, key) for entry in entries for key in keys):
        raise SignatureError("no valid webhook signature")
    return webhook_id


def sign_webhook(
    private_keys: Ed25519PrivateKey | Sequence[Ed25519PrivateKey],
    webhook_id: str,
    timestamp: int,
    body: bytes,
) -> dict[str, str]:
    """Returns Standard Webhooks `v1a` headers, as the service signs a delivery: one signature
    per key, as during a rotation.

    For test senders only: a merchant never holds its account's webhook key.
    """
    keys = [private_keys] if isinstance(private_keys, Ed25519PrivateKey) else private_keys
    content = f"{webhook_id}.{timestamp}.".encode() + body
    signatures = [base64.b64encode(key.sign(content)).decode("ascii") for key in keys]
    return {
        "webhook-id": webhook_id,
        "webhook-timestamp": str(timestamp),
        "webhook-signature": " ".join(f"v1a,{signature}" for signature in signatures),
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
