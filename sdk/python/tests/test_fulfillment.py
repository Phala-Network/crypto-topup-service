from __future__ import annotations

import json
import uuid
from typing import Any

import httpx
import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

from topup_sdk import (
    CREDITED_EVENT,
    CreditedDeposit,
    FulfillmentError,
    SignatureError,
    WebhookEvent,
    credited_event_id,
    sign_webhook,
    verify_webhook,
)
from topup_sdk.__main__ import send_test_event

from .test_webhooks import RUST_BODY, RUST_ID, RUST_KEY, RUST_SIGNATURE, RUST_TIMESTAMP

DEPOSIT = uuid.UUID("3f1c2b9e-6a8d-5c47-9e21-0b7d4f6a8c13")


def _data(**overrides: Any) -> dict[str, Any]:
    data: dict[str, Any] = {
        "product_id": "8b0f7b4e-0000-4000-8000-000000000001",
        "external_id": "team-42",
        "deposit_id": str(DEPOSIT),
        "state": "credited",
        "unit": "USD",
        "amount_minor": "1234",
        "price_source": "lock",
        "price_scaled": "12345678",
        "price_scale": 8,
        "valuation_at": "2026-09-26T12:00:00Z",
        "product_lock_ref": "q-981",
        "address": "0x" + "11" * 20,
        "route": "ethereum-pha",
        "route_version": 1,
        "chain_id": 1,
        "asset_contract": "0x" + "22" * 20,
        "tx_hash": "0x" + "33" * 32,
        "log_index": 12,
        "amount_atomic": "1000000000000000000",
    }
    data.update(overrides)
    return data


def _event(data: dict[str, Any], event_type: str = CREDITED_EVENT) -> WebhookEvent:
    return WebhookEvent(
        id=str(credited_event_id(DEPOSIT)), type=event_type, created_at="…", data=data
    )


def test_credited_event_id_is_derived_from_the_deposit_id() -> None:
    # The service derives the same value (crates/topup, deposit.credited event id vector).
    assert credited_event_id(DEPOSIT) == uuid.UUID("26a20351-ab10-595a-852f-9c1aa0372d73")


def test_credited_event_parses_into_a_typed_credit() -> None:
    credit = CreditedDeposit.from_event(_event(_data()))
    assert credit.deposit_id == DEPOSIT
    assert credit.external_id == "team-42"
    assert credit.amount_minor == 1234
    assert credit.price_source == "lock"
    assert credit.product_lock_ref == "q-981"
    assert credit.fulfillment_key == f"deposit:{DEPOSIT}"


def test_spot_credit_may_carry_no_lock_ref() -> None:
    credit = CreditedDeposit.from_event(_event(_data(price_source="spot", product_lock_ref=None)))
    assert credit.price_source == "spot"
    assert credit.product_lock_ref is None


@pytest.mark.parametrize(
    "event",
    [
        _event(_data(), event_type="deposit.confirmed"),
        # A pre-fulfillment deposit.credited carried destination_tx_id and no account.
        _event({key: value for key, value in _data().items() if key != "external_id"}),
        _event(_data(state="swept")),
        _event(_data(price_source="quote")),
        _event(_data(amount_minor="12.34")),
        _event(_data(amount_minor=1234)),
        _event(_data(deposit_id="not-a-uuid")),
        _event(_data(log_index=-1)),
        _event(_data(route_version=True)),
        _event(_data(product_lock_ref=7)),
    ],
)
def test_other_shapes_are_refused(event: WebhookEvent) -> None:
    with pytest.raises(FulfillmentError):
        CreditedDeposit.from_event(event)


def test_sign_webhook_reproduces_the_service_signature() -> None:
    headers = sign_webhook(RUST_KEY, RUST_ID, RUST_TIMESTAMP, RUST_BODY)
    assert headers["webhook-signature"] == RUST_SIGNATURE


def _receiver(key: Ed25519PrivateKey, credits: dict[str, int]) -> httpx.MockTransport:
    def handle(request: httpx.Request) -> httpx.Response:
        try:
            event = verify_webhook(request.headers, request.content, key.public_key())
        except SignatureError:
            return httpx.Response(400)
        credit = CreditedDeposit.from_event(event)
        credits.setdefault(credit.fulfillment_key, credit.amount_minor)
        return httpx.Response(204)

    return httpx.MockTransport(handle)


def test_send_test_event_passes_a_verifying_deduplicating_receiver() -> None:
    key = Ed25519PrivateKey.generate()
    credits: dict[str, int] = {}
    report = send_test_event(
        "https://product.example/webhooks",
        key,
        external_id="team-42",
        amount_minor=250,
        product_id=uuid.UUID(int=0),
        transport=_receiver(key, credits),
    )
    assert report["passed"], json.dumps(report)
    assert [result["status"] for result in report["results"]] == [204, 204, 400]
    assert credits == {f"deposit:{report['deposit_id']}": 250}
    assert report["event_id"] == str(credited_event_id(uuid.UUID(report["deposit_id"])))


def test_send_test_event_fails_a_receiver_that_skips_verification() -> None:
    report = send_test_event(
        "https://product.example/webhooks",
        Ed25519PrivateKey.generate(),
        external_id="team-42",
        amount_minor=250,
        product_id=uuid.UUID(int=0),
        transport=httpx.MockTransport(lambda _request: httpx.Response(200)),
    )
    assert not report["passed"]
    assert report["results"][2] == {"case": "foreign_signature", "status": 200, "ok": False}
