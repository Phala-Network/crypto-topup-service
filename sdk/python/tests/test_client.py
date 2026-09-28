from __future__ import annotations

import json
import uuid
from collections.abc import Callable

import httpx
import pytest

from topup_client.models import QuotePayment
from topup_sdk import (
    AddressMismatchError,
    ApiError,
    TopupClient,
    forwarder_address,
    lock_salt,
)

API_KEY = "ppay_sk_test_" + "A" * 43 + "000000"
ACCOUNT = "acct_" + "0a" * 16
NOW = 1_790_000_000
FACTORY = "0x" + "aa" * 20
IMPLEMENTATION = "0x" + "bb" * 20
TREASURY = "0x" + "cc" * 20
QUOTE_ID = "qt_" + "0c" * 16


def _quote(**fields: object) -> dict[str, object]:
    address = forwarder_address(
        FACTORY, IMPLEMENTATION, TREASURY, lock_salt(ACCOUNT, "ws 1", QUOTE_ID)
    )
    return {
        "id": QUOTE_ID,
        "object": "quote",
        "account_id": "ws 1",
        "amount": 2500,
        "currency": "usd",
        "chain_id": 11155111,
        "asset": "pha",
        "amount_atomic": "100",
        "exchange_rate": "25.00000000",
        "address": address,
        "payment_uri": f"ethereum:0x{'22' * 20}@11155111/transfer?address={address}&uint256=100",
        "status": "open",
        "expires_at": NOW + 900,
        "created": NOW,
        "payment": None,
        "deposit": None,
        **fields,
    }


class FakeService:
    """Checks each request's API key like the service and records it."""

    def __init__(self, respond: Callable[[httpx.Request, int], httpx.Response]) -> None:
        self.respond = respond
        self.requests: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        if request.headers.get("authorization") != f"Bearer {API_KEY}":
            return _error(401, "api_key_invalid")
        if request.url.raw_path == b"/v1/account":
            return httpx.Response(
                200,
                json={
                    "id": ACCOUNT,
                    "object": "account",
                    "livemode": False,
                    "name": "Acme",
                    "charges_enabled": False,
                    "paused_scopes": [],
                    "created": NOW,
                },
            )
        self.requests.append(request)
        return self.respond(request, len(self.requests))


def _error(status: int, code: str, **fields: str) -> httpx.Response:
    error_type = "api_error" if status >= 500 else "invalid_request_error"
    return httpx.Response(
        status, json={"error": {"type": error_type, "code": code, "message": code, **fields}}
    )


def _client(service: FakeService, *, pinned: bool = True, max_attempts: int = 4) -> TopupClient:
    return TopupClient(
        "http://service.test:8080",
        API_KEY,
        forwarder=(FACTORY, IMPLEMENTATION, TREASURY) if pinned else None,
        transport=httpx.MockTransport(service),
        sleep=lambda _: None,
        max_attempts=max_attempts,
    )


def test_quotes_send_the_key_and_an_idempotency_key() -> None:
    service = FakeService(lambda request, _: httpx.Response(200, json=_quote()))
    with _client(service) as client:
        quote = client.create_quote("ws 1", 2500, chain_id=11155111, asset="pha")
        # The address check read the account id once.
        assert client.account_id() == ACCOUNT
    assert quote.id == QUOTE_ID
    request = service.requests[0]
    assert request.url.raw_path == b"/v1/quotes"
    assert json.loads(request.content) == {
        "account_id": "ws 1",
        "amount": 2500,
        "currency": "usd",
        "chain_id": 11155111,
        "asset": "pha",
    }
    assert uuid.UUID(request.headers["idempotency-key"].strip('"'))
    assert request.headers["authorization"] == f"Bearer {API_KEY}"
    assert "signature" not in request.headers


def test_transient_failures_are_retried_with_one_idempotency_key() -> None:
    def respond(request: httpx.Request, count: int) -> httpx.Response:
        if count == 1:
            return _error(503, "unavailable")
        if count == 2:
            return _error(409, "idempotency_key_in_use")
        return httpx.Response(200, json=_quote())

    service = FakeService(respond)
    with _client(service) as client:
        client.create_quote("ws 1", 2500, chain_id=11155111, asset="pha", idempotency_key="k-1")
    assert len(service.requests) == 3
    assert {request.headers["idempotency-key"] for request in service.requests} == {'"k-1"'}


def test_business_errors_are_raised_without_retry() -> None:
    service = FakeService(lambda request, _: _error(409, "exposure_cap_exceeded", param="amount"))
    with _client(service) as client, pytest.raises(ApiError) as raised:
        client.create_quote("ws 1", 2500, chain_id=11155111, asset="pha")
    error = raised.value
    assert (error.status_code, error.code, error.param) == (409, "exposure_cap_exceeded", "amount")
    assert error.error_type == "invalid_request_error"
    assert len(service.requests) == 1


def test_retries_stop_after_max_attempts() -> None:
    service = FakeService(lambda request, _: _error(503, "unavailable"))
    with _client(service, max_attempts=2) as client, pytest.raises(ApiError):
        client.get_quote(QUOTE_ID)
    assert len(service.requests) == 2


def test_open_quotes_must_have_the_derived_address() -> None:
    forged = _quote(address="0x" + "11" * 20)
    service = FakeService(lambda request, _: httpx.Response(200, json=forged))
    with _client(service) as client, pytest.raises(AddressMismatchError):
        client.get_quote(QUOTE_ID)
    # A closed quote's address is not shown, and without a pinned forwarder nothing is checked.
    closed = FakeService(
        lambda request, _: httpx.Response(200, json={**forged, "status": "expired"})
    )
    with _client(closed) as client:
        assert client.get_quote(QUOTE_ID).status == "expired"
    with _client(
        FakeService(lambda request, _: httpx.Response(200, json=forged)), pinned=False
    ) as client:
        assert client.get_quote(QUOTE_ID).address == forged["address"]


def test_only_secret_keys_are_accepted() -> None:
    for key in ["sk_test_123", "ppay_rk_test_" + "A" * 49, "acme/v1"]:
        with pytest.raises(ValueError, match="secret key"):
            TopupClient("http://service.test", key)


def _deposit(index: int) -> dict[str, object]:
    return {
        "id": f"dep_{index:032x}",
        "object": "deposit",
        "account_id": "ws 1",
        "quote": QUOTE_ID,
        "status": "credited",
        "rejection_reason": None,
        "chain_id": 11155111,
        "asset": "pha",
        "asset_contract": "0x" + "22" * 20,
        "amount_atomic": "1",
        "amount": 1,
        "currency": "usd",
        "exchange_rate": "1.00000000",
        "price_source": "quote",
        "valued_at": NOW,
        "address": "0x" + "11" * 20,
        "from_address": "0x" + "33" * 20,
        "tx_hash": "0x" + "ab" * 32,
        "log_index": index,
        "block_number": 1,
        "amount_refunded_atomic": "0",
        "refunded": False,
        "created": NOW,
    }


def test_list_deposits_follows_stripe_cursors() -> None:
    def respond(request: httpx.Request, count: int) -> httpx.Response:
        params = request.url.params
        assert request.url.raw_path.startswith(b"/v1/deposits")
        assert params["account_id"] == "ws 1"
        assert params["status"] == "credited"
        if count == 1:
            assert "starting_after" not in params
            return httpx.Response(
                200,
                json={
                    "object": "list",
                    "url": "/v1/deposits",
                    "has_more": True,
                    "data": [_deposit(1)],
                },
            )
        assert params["starting_after"] == f"dep_{1:032x}"
        return httpx.Response(
            200,
            json={
                "object": "list",
                "url": "/v1/deposits",
                "has_more": False,
                "data": [_deposit(2)],
            },
        )

    service = FakeService(respond)
    with _client(service) as client:
        deposits = list(client.list_deposits(account_id="ws 1", status="credited"))
    assert [deposit.log_index for deposit in deposits] == [1, 2]
    assert deposits[0].quote == QUOTE_ID


REFUND_ID = "re_" + "0e" * 16
REFUND = {
    "id": REFUND_ID,
    "object": "refund",
    "deposit": f"dep_{1:032x}",
    "amount_atomic": "1",
    "destination_address": "0x" + "44" * 20,
    "treasury": "0x" + "7e" * 20,
    "status": "pending",
    "failure_reason": None,
    "transaction_hash": None,
    "log_index": None,
    "created": NOW,
}


def test_refunds_send_an_idempotency_key_and_default_to_the_remainder() -> None:
    service = FakeService(lambda request, _: httpx.Response(200, json=REFUND))
    with _client(service) as client:
        created = client.create_refund(f"dep_{1:032x}", "0x" + "44" * 20)
    assert created.status == "pending"
    assert created.treasury == "0x" + "7e" * 20
    request = service.requests[0]
    assert request.url.raw_path == b"/v1/refunds"
    assert json.loads(request.content) == {
        "deposit": f"dep_{1:032x}",
        "destination_address": "0x" + "44" * 20,
    }
    assert request.headers["idempotency-key"].startswith('"')


def test_refunds_are_marked_paid_and_canceled_by_id() -> None:
    marked = {**REFUND, "transaction_hash": "0x" + "dd" * 32, "log_index": 7}
    service = FakeService(lambda request, _: httpx.Response(200, json=marked))
    with _client(service) as client:
        refund = client.mark_refund_paid(REFUND_ID, "0x" + "dd" * 32, log_index=7)
        client.mark_refund_paid(REFUND_ID, "0x" + "dd" * 32)
        client.cancel_refund(REFUND_ID)
    assert refund.transaction_hash == "0x" + "dd" * 32
    assert refund.log_index == 7
    paid, unnamed, canceled = service.requests
    assert paid.url.raw_path == f"/v1/refunds/{REFUND_ID}/mark_paid".encode()
    assert json.loads(paid.content) == {"transaction_hash": "0x" + "dd" * 32, "log_index": 7}
    assert json.loads(unnamed.content) == {"transaction_hash": "0x" + "dd" * 32}
    assert canceled.method == "POST"
    assert canceled.url.raw_path == f"/v1/refunds/{REFUND_ID}/cancel".encode()


def test_quote_payment_is_optional_and_parsed() -> None:
    payment = {
        "status": "seen",
        "tx_hash": "0x" + "ab" * 32,
        "amount_atomic": "100",
        "confirmations": 1,
        "estimated_final_at": NOW + 900,
        "matches_quote": True,
        "deposit": "dep_" + "08" * 16,
    }

    def respond(_: httpx.Request, count: int) -> httpx.Response:
        return httpx.Response(200, json=_quote() if count == 1 else _quote(payment=payment))

    with _client(FakeService(respond)) as client:
        unpaid = client.get_quote(QUOTE_ID)
        seen = client.get_quote(QUOTE_ID)
    assert not isinstance(unpaid.payment, QuotePayment)
    assert isinstance(seen.payment, QuotePayment)
    assert seen.payment.status == "seen"
    assert seen.payment.confirmations == 1
    assert seen.payment.matches_quote
