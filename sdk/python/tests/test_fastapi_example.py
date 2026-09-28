"""The FastAPI example: quote creation for the checkout and idempotent webhook fulfillment."""

from __future__ import annotations

import json
import sqlite3
import sys
import time
from pathlib import Path

import httpx
import pytest
from fastapi.testclient import TestClient

from phala_pay import PhalaPay
from topup_sdk import sign_webhook

from .test_phala_pay import (
    ACCOUNT,
    API_KEY,
    EVENT_ID,
    QUOTE_ID,
    SERVICE_KEY,
    SERVICE_PUBLIC_KEY,
    _deposit,
)
from .test_phala_pay import _quote as quote_object

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "examples"))

from fastapi_app import create_app

SECRET = f"{QUOTE_ID}_secret_{'ab' * 24}"


@pytest.fixture
def app(tmp_path: Path) -> tuple[TestClient, list[httpx.Request], Path]:
    requests: list[httpx.Request] = []

    def service(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        body = json.loads(request.content)
        if body["amount"] < 100:
            return httpx.Response(
                400,
                json={
                    "error": {
                        "type": "invalid_request_error",
                        "code": "amount_too_small",
                        "message": "internal detail",
                        "param": "amount",
                    }
                },
            )
        return httpx.Response(200, json=quote_object(client_secret=SECRET))

    client = PhalaPay("http://service.test", API_KEY, transport=httpx.MockTransport(service))
    database = tmp_path / "product.sqlite3"
    api = create_app(
        client,
        [SERVICE_PUBLIC_KEY],
        str(database),
        account=ACCOUNT,
        livemode=False,
        chain_id=11155111,
        asset="pha",
    )
    return TestClient(api), requests, database


def test_topup_returns_the_client_secret_and_keys_the_quote_by_order(
    app: tuple[TestClient, list[httpx.Request], Path],
) -> None:
    http, requests, _ = app
    response = http.post("/topups", json={"amount": 2500}, headers={"x-team-id": "team-42"})
    assert response.status_code == 200
    assert response.json()["client_secret"] == SECRET
    sent = requests[0]
    assert json.loads(sent.content)["account_id"] == "team-42"
    assert sent.headers["idempotency-key"] == f'"{response.json()["order_id"]}"'


def test_topup_passes_on_only_the_error_code(
    app: tuple[TestClient, list[httpx.Request], Path],
) -> None:
    http, _, _ = app
    response = http.post("/topups", json={"amount": 99}, headers={"x-team-id": "team-42"})
    assert response.status_code == 400
    assert response.json() == {"detail": {"code": "amount_too_small"}}
    assert http.post("/topups", json={"amount": 0}, headers={"x-team-id": "t"}).status_code == 422
    assert http.post("/topups", json={"amount": 100}).status_code == 422


def _credited(body_amount: int = 2500, account: str = ACCOUNT) -> tuple[bytes, dict[str, str]]:
    body = json.dumps(
        {
            "id": EVENT_ID,
            "object": "event",
            "account": account,
            "livemode": False,
            "type": "deposit.credited",
            "created": 1_790_000_321,
            "data": {"object": _deposit() | {"amount": body_amount}},
        }
    ).encode()
    return body, sign_webhook(SERVICE_KEY, EVENT_ID, int(time.time()), body)


def test_webhook_credits_each_deposit_once(
    app: tuple[TestClient, list[httpx.Request], Path],
) -> None:
    http, _, database = app
    body, headers = _credited()
    for _ in range(2):
        response = http.post("/webhooks/phala-pay", content=body, headers=headers)
        assert response.status_code == 200
    with sqlite3.connect(database) as db:
        assert db.execute("SELECT team, amount FROM balances").fetchall() == [("team-42", 2500)]
        assert db.execute("SELECT count(*) FROM credits").fetchone() == (1,)


def test_webhook_refuses_a_forged_delivery(
    app: tuple[TestClient, list[httpx.Request], Path],
) -> None:
    http, _, database = app
    body, headers = _credited()
    forged = body.replace(b'"amount": 2500', b'"amount": 999999')
    assert http.post("/webhooks/phala-pay", content=forged, headers=headers).status_code == 400
    assert http.post("/webhooks/phala-pay", content=body).status_code == 400
    # Signed with the pinned key, but another account's event: refused.
    other_body, other_headers = _credited(account="acct_" + "b2" * 16)
    response = http.post("/webhooks/phala-pay", content=other_body, headers=other_headers)
    assert response.status_code == 400
    with sqlite3.connect(database) as db:
        assert db.execute("SELECT count(*) FROM balances").fetchone() == (0,)
