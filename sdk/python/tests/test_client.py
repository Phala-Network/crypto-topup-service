from __future__ import annotations

import json
import uuid
from collections.abc import Callable

import httpx
import pytest

from topup_client.models import RateLockPayment
from topup_sdk import ApiError, RequestSigner, TopupClient, load_public_key, verify_request
from topup_sdk.signing import target_uri

SEED = bytes([5] * 32)
KEYID = "acme/v1"
NOW = 1_790_000_000
ACCOUNT = {
    "id": str(uuid.UUID(int=1)),
    "external_id": "ws 1",
    "status": "active",
    "paused_scopes": [],
}


class FakeService:
    """Verifies each request like the service and enforces single-use signatures."""

    def __init__(self, respond: Callable[[httpx.Request, int], httpx.Response]) -> None:
        self.respond = respond
        self.requests: list[httpx.Request] = []
        self.signatures: set[str] = set()
        signer = RequestSigner.from_seed(KEYID, SEED)
        self.public_key = load_public_key(signer.public_key_base64())

    def __call__(self, request: httpx.Request) -> httpx.Response:
        verify_request(
            method=request.method,
            target_uri=target_uri(request),
            headers=dict(request.headers),
            body=request.content,
            public_key=self.public_key,
            keyid=KEYID,
            require_idempotency_key=False,
            now=NOW,
        )
        signature = request.headers["signature"]
        if signature in self.signatures:
            return _error(409, "signature_replayed")
        self.signatures.add(signature)
        self.requests.append(request)
        return self.respond(request, len(self.requests))


def _error(status: int, code: str) -> httpx.Response:
    return httpx.Response(status, json={"error": {"code": code, "message": code}})


def _client(service: FakeService, **kwargs: int) -> TopupClient:
    signer = RequestSigner.from_seed(KEYID, SEED, clock=lambda: NOW)
    return TopupClient(
        "http://service.test:8080",
        "acme",
        signer,
        transport=httpx.MockTransport(service),
        sleep=lambda _: None,
        **kwargs,
    )


def test_requests_are_signed_and_percent_encoded() -> None:
    service = FakeService(lambda request, _: httpx.Response(200, json=ACCOUNT))
    with _client(service) as client:
        account = client.register_account("ws 1")
    assert account.external_id == "ws 1"
    request = service.requests[0]
    assert request.url.raw_path == b"/v1/products/acme/accounts"
    assert json.loads(request.content) == {"external_id": "ws 1"}
    assert "authorization" not in request.headers


def test_transient_failures_are_retried_with_fresh_signatures() -> None:
    def respond(request: httpx.Request, count: int) -> httpx.Response:
        return (
            _error(503, "pricing_unavailable") if count < 3 else httpx.Response(200, json=ACCOUNT)
        )

    service = FakeService(respond)
    with _client(service) as client:
        client.register_account("ws 1")
    # Three identical requests within one clock second: each carries a distinct signature.
    assert len(service.requests) == 3
    assert len(service.signatures) == 3


def test_business_errors_are_raised_without_retry() -> None:
    service = FakeService(lambda request, _: _error(409, "exposure_cap_exceeded"))
    with _client(service) as client, pytest.raises(ApiError) as raised:
        client.create_rate_lock("ws 1", "checkout-1", amount_minor=2500)
    assert (raised.value.status_code, raised.value.code) == (409, "exposure_cap_exceeded")
    assert len(service.requests) == 1
    assert json.loads(service.requests[0].content) == {
        "product_lock_ref": "checkout-1",
        "amount_minor": "2500",
    }


def test_retries_stop_after_max_attempts() -> None:
    service = FakeService(lambda request, _: _error(503, "pricing_unavailable"))
    with _client(service, max_attempts=2) as client, pytest.raises(ApiError):
        client.get_limits("ws 1")
    assert len(service.requests) == 2


def test_rate_lock_requires_exactly_one_amount() -> None:
    service = FakeService(lambda request, _: _error(500, "unused"))
    with _client(service) as client, pytest.raises(ValueError, match="exactly one"):
        client.create_rate_lock("ws 1", "checkout-1", amount_minor=1, amount_atomic=1)


def _deposit(index: int) -> dict[str, object]:
    return {
        "id": str(uuid.UUID(int=index)),
        "chain_id": 11155111,
        "tx_hash": "0x" + "ab" * 32,
        "log_index": index,
        "block_number": 1,
        "block_time": "2026-09-22T00:00:00Z",
        "address": "0x" + "11" * 20,
        "asset_contract": "0x" + "22" * 20,
        "from_address": "0x" + "33" * 20,
        "amount_atomic": "1",
        "state": "credited",
        "created_at": "2026-09-22T00:00:00Z",
        "updated_at": "2026-09-22T00:00:00Z",
    }


def test_list_deposits_follows_cursors() -> None:
    cursor = str(uuid.UUID(int=99))

    def respond(request: httpx.Request, count: int) -> httpx.Response:
        if count == 1:
            assert "cursor" not in request.url.params
            return httpx.Response(200, json={"deposits": [_deposit(1)], "next_cursor": cursor})
        assert request.url.params["cursor"] == cursor
        assert request.url.params["state"] == "credited"
        return httpx.Response(200, json={"deposits": [_deposit(2)], "next_cursor": None})

    service = FakeService(respond)
    with _client(service) as client:
        deposits = list(client.list_deposits("ws 1", state="credited"))
    assert [deposit.log_index for deposit in deposits] == [1, 2]


def test_list_pending_deposits_returns_provisional_transfers() -> None:
    pending = {
        "deposit_id": str(uuid.UUID(int=7)),
        "chain_id": 11155111,
        "tx_hash": "0x" + "ab" * 32,
        "log_index": 3,
        "block_number": 10,
        "block_time": "2026-09-22T00:00:00Z",
        "confirmations": 2,
        "address": "0x" + "11" * 20,
        "asset_contract": "0x" + "22" * 20,
        "from_address": "0x" + "33" * 20,
        "amount_atomic": "5",
        "supported": False,
        "first_seen_at": "2026-09-22T00:00:05Z",
        "estimated_final_at": "2026-09-22T00:15:00Z",
    }

    def respond(request: httpx.Request, _: int) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.raw_path == b"/v1/products/acme/accounts/ws%201/pending-deposits"
        return httpx.Response(200, json={"pending_deposits": [pending]})

    with _client(FakeService(respond)) as client:
        transfers = client.list_pending_deposits("ws 1")
    assert [(item.confirmations, item.supported) for item in transfers] == [(2, False)]
    assert transfers[0].estimated_final_at.minute == 15


def test_rate_lock_payment_is_optional_and_parsed() -> None:
    lock = {
        "address": "0x" + "11" * 20,
        "amount_atomic": "100",
        "price_scaled": "10000000",
        "credit_minor": "10",
        "expires_at": "2026-09-22T00:30:00Z",
        "status": "open",
        "remaining_seconds": 60,
        "eip681_uri": "ethereum:0x" + "22" * 20 + "@1/transfer",
        "salt_inputs": {"product_slug": "acme", "external_id": "ws 1", "lock_ref": "c-1"},
    }
    payment = {
        "status": "seen",
        "deposit_id": str(uuid.UUID(int=8)),
        "tx_hash": "0x" + "ab" * 32,
        "log_index": 0,
        "block_number": 10,
        "confirmations": 1,
        "amount_atomic": "100",
        "asset_contract": "0x" + "22" * 20,
        "supported": True,
        "amount_within_tolerance": True,
        "in_time": True,
        "estimated_final_at": "2026-09-22T00:15:00Z",
    }

    def respond(_: httpx.Request, count: int) -> httpx.Response:
        body = lock if count == 1 else {**lock, "payment": payment}
        return httpx.Response(200, json=body)

    with _client(FakeService(respond)) as client:
        unpaid = client.get_rate_lock("ws 1", "c-1")
        seen = client.get_rate_lock("ws 1", "c-1")
    assert not isinstance(unpaid.payment, RateLockPayment)
    assert isinstance(seen.payment, RateLockPayment)
    assert seen.payment.status == "seen"
    assert seen.payment.confirmations == 1
    assert seen.payment.amount_within_tolerance
    assert seen.payment.in_time
