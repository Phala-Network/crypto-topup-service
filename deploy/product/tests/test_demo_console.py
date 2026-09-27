"""The Phala Pay demo's API: account cookies, request policy, and the timeline's failure states."""

from __future__ import annotations

import json
import sys
from dataclasses import replace
from http import HTTPStatus
from pathlib import Path
from typing import Any

import httpx
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from reference_product.config import ProductConfig
from reference_product.demo import ApiRecorder, DemoConsole
from reference_product.ledger import ProductLedger
from topup_sdk import forwarder_address, lock_salt

NOW = 1_790_000_000
QUOTE = "qt_" + "0c" * 16
CONFIG = ProductConfig(
    service_url="http://service.test",
    product_slug="acme",
    product_keyid="acme/v1",
    route="sandbox-acme-tpha-usd",
    chain_id=11155111,
    rpc_url="http://rpc.test",
    factory="0x" + "aa" * 20,
    implementation="0x" + "bb" * 20,
    token="0x" + "44" * 20,
    token_symbol="PHA",  # noqa: S106 - an asset symbol, not a secret
    public_url="https://acme.example",
)


def _quote(account: str = "acct", **fields: Any) -> dict[str, Any]:
    address = forwarder_address(
        CONFIG.factory, CONFIG.implementation, lock_salt("acme", account, QUOTE)
    )
    return {
        "id": QUOTE,
        "object": "quote",
        "account_id": account,
        "amount": 2500,
        "currency": "usd",
        "chain_id": 11155111,
        "asset": "pha",
        "amount_atomic": "100",
        "exchange_rate": "25.00000000",
        "address": address,
        "payment_uri": f"ethereum:0x{'44' * 20}@11155111/transfer?address={address}&uint256=100",
        "status": "open",
        "expires_at": NOW + 900,
        "created": NOW,
        "payment": None,
        "deposit": None,
        **fields,
    }


def _deposit(**fields: Any) -> dict[str, Any]:
    return {
        "id": "dep_" + "0d" * 16,
        "object": "deposit",
        "account_id": "acct",
        "quote": QUOTE,
        "status": "credited",
        "rejection_reason": None,
        "chain_id": 11155111,
        "asset": "pha",
        "asset_contract": "0x" + "44" * 20,
        "amount_atomic": "100",
        "amount": 2500,
        "currency": "usd",
        "exchange_rate": "25.00000000",
        "price_source": "quote",
        "valued_at": NOW,
        "address": "0x" + "11" * 20,
        "from_address": "0x" + "33" * 20,
        "tx_hash": "0x" + "ab" * 32,
        "log_index": 0,
        "block_number": 1,
        "amount_refunded_atomic": "0",
        "refunded": False,
        "created": NOW,
        **fields,
    }


class Service:
    def __init__(self) -> None:
        self.quote = _quote()
        self.deposits: list[dict[str, Any]] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        if request.url.path == "/v1/quotes":
            self.quote = _quote(json.loads(request.content)["account_id"])
            quote = {**self.quote, "client_secret": f"{QUOTE}_secret_{'ab' * 24}"}
            return httpx.Response(200, json=quote)
        if request.url.path.startswith("/v1/quotes/"):
            return httpx.Response(200, json=self.quote)
        list_body = {"object": "list", "url": "/v1/deposits", "has_more": False}
        return httpx.Response(200, json={**list_body, "data": self.deposits})


@pytest.fixture
def demo(tmp_path: Path) -> tuple[DemoConsole, Service]:
    (tmp_path / "index.html").write_text("<!doctype html>")
    (tmp_path / "secret.txt").write_text("not served")
    (tmp_path / "product.seed").write_text("00" * 32)
    service = Service()
    console = DemoConsole(
        replace(CONFIG, product_seed_file=str(tmp_path / "product.seed")),
        ProductLedger(),
        tmp_path,
        recorder=ApiRecorder(httpx.MockTransport(service)),
        clock=lambda: NOW,
    )
    return console, service


def _account(console: DemoConsole) -> str:
    response = console.handle("GET", "/demo/api/account", {}, b"")
    assert response.status == HTTPStatus.OK
    cookie = response.headers["set-cookie"]
    assert "HttpOnly" in cookie
    assert "SameSite=Strict" in cookie
    assert "Secure" in cookie
    return cookie.split(";")[0]


def _create_quote(console: DemoConsole, cookie: str) -> None:
    headers = {"Cookie": cookie, "Content-Type": "application/json"}
    response = console.handle("POST", "/demo/api/quotes", headers, b'{"amount": 2500}')
    assert response.status == HTTPStatus.OK
    assert json.loads(response.body)["client_secret"].startswith(QUOTE)


def _steps(console: DemoConsole, cookie: str) -> dict[str, str]:
    response = console.handle("GET", f"/demo/api/quotes/{QUOTE}", {"Cookie": cookie}, b"")
    assert response.status == HTTPStatus.OK
    return {step["key"]: step["state"] for step in json.loads(response.body)["steps"]}


def test_an_expired_quote_without_payment_fails_at_the_transfer(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    _create_quote(console, cookie)
    service.quote = {**service.quote, "expires_at": NOW - 1}
    steps = _steps(console, cookie)
    assert steps["transfer_seen"] == "failed"
    assert steps["finalized"] == "upcoming"


def test_a_rejected_deposit_fails_at_the_credit(demo: tuple[DemoConsole, Service]) -> None:
    console, service = demo
    cookie = _account(console)
    _create_quote(console, cookie)
    service.deposits = [_deposit(status="rejected", rejection_reason="sanctioned", amount=None)]
    steps = _steps(console, cookie)
    assert steps["transfer_seen"] == "complete"
    assert steps["credited"] == "failed"
    assert steps["webhook_received"] == "upcoming"


def test_a_credited_deposit_waits_for_the_webhook(demo: tuple[DemoConsole, Service]) -> None:
    console, service = demo
    cookie = _account(console)
    _create_quote(console, cookie)
    service.deposits = [_deposit()]
    steps = _steps(console, cookie)
    assert steps["credited"] == "complete"
    assert steps["webhook_received"] == "current"
    assert steps["swept"] == "current"


def test_requests_need_the_cookie_and_quotes_need_json(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    no_cookie = console.handle("GET", f"/demo/api/quotes/{QUOTE}", {}, b"")
    assert no_cookie.status == HTTPStatus.UNAUTHORIZED
    forged = console.handle("GET", "/demo/api/account", {"Cookie": "demo_account=admin"}, b"")
    assert "set-cookie" in forged.headers
    cookie = _account(console)
    form = {"Cookie": cookie, "Content-Type": "application/x-www-form-urlencoded"}
    post = console.handle("POST", "/demo/api/quotes", form, b"amount=2500")
    assert post.status == HTTPStatus.UNSUPPORTED_MEDIA_TYPE


def test_serves_only_the_built_page(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    page = console.handle("GET", "/demo/", {}, b"")
    assert "connect-src 'self' http://service.test;" in page.headers["content-security-policy"]
    for path in ["/demo/secret.txt", "/demo/../config.json", "/demo/api/unknown"]:
        assert console.handle("GET", path, {}, b"").status in (
            HTTPStatus.NOT_FOUND,
            HTTPStatus.UNAUTHORIZED,
        )
