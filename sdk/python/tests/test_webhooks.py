from __future__ import annotations

import base64
import json
from collections.abc import Mapping

import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from topup_sdk import SignatureError, verify_webhook, verify_webhook_signature

# Fixed vector from crates/topup/src/outbox/signature.rs (seed [7; 32]).
RUST_KEY = Ed25519PrivateKey.from_private_bytes(bytes([7] * 32))
RUST_ID = "018d5f8e-8a7b-7d65-bc44-2c4f5f0a6d31"
RUST_TIMESTAMP = 1_674_087_231
RUST_BODY = b'{"type":"deposit.confirmed","data":{"deposit_id":"dep_123"}}'
RUST_SIGNATURE = (
    "v1a,0thypM6abf9ly803QGttAKGQfPFKHiwgpxF+b4zWDUCycKswAoJ848WmI7VKQBw8NIWO74zYeRvd7vw/cGOZBw=="
)


def _rust_headers(signature: str = RUST_SIGNATURE) -> dict[str, str]:
    return {
        "Webhook-Id": RUST_ID,
        "Webhook-Timestamp": str(RUST_TIMESTAMP),
        "Webhook-Signature": signature,
    }


def test_rust_signed_delivery_verifies() -> None:
    webhook_id = verify_webhook_signature(
        _rust_headers(), RUST_BODY, RUST_KEY.public_key(), now=RUST_TIMESTAMP
    )
    assert webhook_id == RUST_ID


def test_any_listed_v1a_signature_may_match() -> None:
    headers = _rust_headers(f"v1,c29tZXRoaW5n v1a,AAAA {RUST_SIGNATURE}")
    verify_webhook_signature(headers, RUST_BODY, RUST_KEY.public_key(), now=RUST_TIMESTAMP)


@pytest.mark.parametrize(
    ("headers", "body", "now"),
    [
        (_rust_headers(), RUST_BODY + b" ", RUST_TIMESTAMP),
        (_rust_headers(), RUST_BODY, RUST_TIMESTAMP + 301),
        (_rust_headers(), RUST_BODY, RUST_TIMESTAMP - 301),
        ({**_rust_headers(), "Webhook-Id": "other"}, RUST_BODY, RUST_TIMESTAMP),
        (_rust_headers(RUST_SIGNATURE.replace("v1a,", "v1,")), RUST_BODY, RUST_TIMESTAMP),
        ({"Webhook-Id": RUST_ID}, RUST_BODY, RUST_TIMESTAMP),
        # Non-ASCII digits pass str.isdigit() but are not a Unix timestamp.
        ({**_rust_headers(), "Webhook-Timestamp": "\u0661\u0662"}, RUST_BODY, RUST_TIMESTAMP),
    ],
)
def test_tampered_or_stale_deliveries_are_rejected(
    headers: dict[str, str], body: bytes, now: int
) -> None:
    with pytest.raises(SignatureError):
        verify_webhook_signature(headers, body, RUST_KEY.public_key(), now=now)


def _signed(envelope: Mapping[str, object], webhook_id: str) -> tuple[dict[str, str], bytes]:
    body = json.dumps(envelope).encode()
    signature = RUST_KEY.sign(f"{webhook_id}.{RUST_TIMESTAMP}.".encode() + body)
    headers = {
        "webhook-id": webhook_id,
        "webhook-timestamp": str(RUST_TIMESTAMP),
        "webhook-signature": "v1a," + base64.b64encode(signature).decode(),
    }
    return headers, body


def test_envelope_is_parsed_and_bound_to_the_webhook_id() -> None:
    envelope = {
        "event_id": RUST_ID,
        "type": "deposit.credited",
        "created_at": "2026-09-22T00:00:00Z",
        "data": {"deposit_id": "d"},
    }
    headers, body = _signed(envelope, RUST_ID)
    event = verify_webhook(headers, body, RUST_KEY.public_key(), now=RUST_TIMESTAMP)
    assert (event.id, event.type, event.data) == (RUST_ID, "deposit.credited", {"deposit_id": "d"})

    headers, body = _signed({**envelope, "event_id": "another"}, RUST_ID)
    with pytest.raises(SignatureError, match="does not match"):
        verify_webhook(headers, body, RUST_KEY.public_key(), now=RUST_TIMESTAMP)
