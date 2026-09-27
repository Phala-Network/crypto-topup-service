"""`Webhook.construct_event`: verify a delivery and return the typed event, as Stripe's does."""

from __future__ import annotations

import json
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import Deposit, Quote
from topup_sdk import SignatureError, load_public_key, verify_webhook_signature

DEFAULT_TOLERANCE = 300


class SignatureVerificationError(SignatureError):
    """The delivery's signature, timestamp, or id did not verify; answer `400` and do nothing."""


@dataclass(frozen=True)
class EventData:
    """`object` is the resource as it was when the event happened: a `Deposit` for `deposit.*`,
    a `Quote` for `quote.*`, and the raw object for any other type."""

    object: Deposit | Quote | dict[str, Any]


@dataclass(frozen=True)
class Event:
    """A verified event: `deposit.credited`, `deposit.rejected`, `deposit.reversed`,
    `deposit.refunded`, or `quote.expired`. Its `id` is stable across retries and replays;
    process each id once. Claw back the credit of a `deposit.reversed` deposit as for
    `deposit.refunded`."""

    id: str
    type: str
    created: int
    data: EventData

    @property
    def deposit(self) -> Deposit:
        """`data.object` of a `deposit.*` event."""
        if not isinstance(self.data.object, Deposit):
            raise TypeError(f"{self.type} does not carry a deposit")
        return self.data.object

    @property
    def quote(self) -> Quote:
        """`data.object` of a `quote.*` event."""
        if not isinstance(self.data.object, Quote):
            raise TypeError(f"{self.type} does not carry a quote")
        return self.data.object


class Webhook:
    @staticmethod
    def construct_event(
        payload: bytes | str,
        headers: Mapping[str, str],
        public_key: str | Ed25519PublicKey,
        *,
        tolerance: int = DEFAULT_TOLERANCE,
    ) -> Event:
        """Verifies a delivery and returns its event.

        `payload` is the raw request body, before any JSON parsing; `headers` are the request
        headers (`webhook-id`, `webhook-timestamp`, `webhook-signature`); `public_key` is the
        service's settlement key (hex or base64), pinned from attestation. Raises
        `SignatureVerificationError` when the signature does not verify, the timestamp is more
        than `tolerance` seconds away, or the body's id differs from `webhook-id`, and
        `ValueError` when a verified body is not an event. That includes an operator replay of
        an event written before `evt_` ids (a UUID `webhook-id` and a flat body): those predate
        this SDK and were fulfilled when first delivered; `topup_sdk.verify_webhook` reads them.
        """
        body = payload.encode() if isinstance(payload, str) else payload
        key = load_public_key(public_key) if isinstance(public_key, str) else public_key
        try:
            webhook_id = verify_webhook_signature(headers, body, key, tolerance_seconds=tolerance)
        except SignatureError as error:
            raise SignatureVerificationError(str(error)) from error

        envelope = json.loads(body)
        if not isinstance(envelope, dict) or envelope.get("object") != "event":
            raise ValueError("webhook body is not an event")
        event_id, event_type, created = (
            envelope.get("id"),
            envelope.get("type"),
            envelope.get("created"),
        )
        data = envelope.get("data")
        if (
            not isinstance(event_id, str)
            or not isinstance(event_type, str)
            or type(created) is not int
            or not isinstance(data, dict)
            or not isinstance(data.get("object"), dict)
        ):
            raise ValueError("webhook body is not an event")
        if event_id != webhook_id:
            raise SignatureVerificationError("webhook id does not match the event")
        return Event(
            event_id, event_type, created, EventData(_resource(event_type, data["object"]))
        )


def _resource(event_type: str, value: dict[str, Any]) -> Deposit | Quote | dict[str, Any]:
    resource = event_type.partition(".")[0]
    try:
        if resource == "deposit":
            return Deposit.from_dict(value)
        if resource == "quote":
            return Quote.from_dict(value)
    except (KeyError, TypeError, ValueError) as error:
        raise ValueError(f"{event_type} carries a malformed {resource}") from error
    return value
