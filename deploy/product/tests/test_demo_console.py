"""The Phala Pay demo's API: account cookies, request policy, the pins, both collection methods,
refunds, sweeps, and the timeline's and ledger's states."""

from __future__ import annotations

import json
import re
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
from topup_sdk import deposit_address_salt, forwarder_address, quote_salt

NOW = 1_790_000_000
ACCOUNT = "acct_" + "ac" * 16
QUOTE = "qt_" + "0c" * 16
DEPOSIT = "dep_" + "0d" * 16
REFUND = "re_" + "0e" * 16
ADDRESS_ID = "da_" + "0a" * 16
CONFIG = ProductConfig(
    service_url="http://service.test",
    account=ACCOUNT,
    route="sandbox-acme-tpha-usd",
    chain_id=11155111,
    rpc_url="http://rpc.test",
    factory="0x" + "aa" * 20,
    implementation="0x" + "bb" * 20,
    treasury="0x" + "cc" * 20,
    token="0x" + "44" * 20,
    token_symbol="PHA",  # noqa: S106 - an asset symbol, not a secret
    public_url="https://acme.example",
)


def _quote(customer: str = "acct", **fields: Any) -> dict[str, Any]:
    address = forwarder_address(
        CONFIG.factory, CONFIG.implementation, CONFIG.treasury, quote_salt(ACCOUNT, customer, QUOTE)
    )
    return {
        "id": QUOTE,
        "object": "quote",
        "livemode": False,
        "client_reference_id": customer,
        "treasury": CONFIG.treasury.lower(),
        "metadata": {"order_id": "order_1"},
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


def _deposit_address(customer: str, *, address: str | None = None) -> dict[str, Any]:
    salt = deposit_address_salt(ACCOUNT, livemode=False, client_reference_id=customer, version=1)
    derived = forwarder_address(CONFIG.factory, CONFIG.implementation, CONFIG.treasury, salt)
    at = address or derived
    return {
        "id": ADDRESS_ID,
        "object": "deposit_address",
        "livemode": False,
        "client_reference_id": customer,
        "address": at,
        "version": 1,
        "salt": "0x" + salt.hex(),
        "status": "active",
        "created": NOW,
        "retired_at": None,
        "metadata": {"workspace": customer},
        "networks": [
            {
                "chain_id": 11155111,
                "address": at,
                "treasury": CONFIG.treasury,
                "assets": [
                    {
                        "asset": "pha",
                        "contract": CONFIG.token,
                        "decimals": 18,
                        "payment_uri": f"ethereum:{CONFIG.token}@11155111/transfer?address={at}",
                    }
                ],
            }
        ],
        "payments": [],
    }


def _deposit(customer: str = "acct", **fields: Any) -> dict[str, Any]:
    return {
        "id": DEPOSIT,
        "object": "deposit",
        "livemode": False,
        "client_reference_id": customer,
        "quote": QUOTE,
        "deposit_address": None,
        "status": "credited",
        "final": True,
        "final_at": NOW + 780,
        "swept": False,
        "metadata": {"order_id": "order_1"},
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
        "amount_refunded": 0,
        "amount_reversed": 0,
        "created": NOW,
        **fields,
    }


def _list(url: str, data: list[dict[str, Any]]) -> dict[str, Any]:
    return {"object": "list", "url": url, "has_more": False, "data": data}


class Service:
    """The service's merchant API, as much of it as the demo reads."""

    def __init__(self) -> None:
        self.quote = _quote()
        self.deposits: list[dict[str, Any]] = []
        self.refunds: list[dict[str, Any]] = []
        self.forwarders: list[dict[str, Any]] = []
        self.wrong_address: str | None = None
        self.customer = "acct"
        self.requests: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        path = request.url.path
        body = json.loads(request.content) if request.content else {}
        if path == "/v1/quotes":
            self.quote = _quote(body["client_reference_id"], metadata=body["metadata"])
            quote = {**self.quote, "client_secret": f"{QUOTE}_secret_{'ab' * 24}"}
            return httpx.Response(200, json=quote)
        if path.startswith("/v1/quotes/"):
            return httpx.Response(200, json=self.quote)
        if path == "/v1/deposit_addresses":
            self.customer = body["client_reference_id"]
            address = _deposit_address(self.customer, address=self.wrong_address)
            return httpx.Response(200, json={**address, "client_secret": f"{ADDRESS_ID}_secret_ab"})
        if path == f"/v1/deposit_addresses/{ADDRESS_ID}":
            return httpx.Response(200, json=_deposit_address(self.customer))
        if path == "/v1/deposits":
            return httpx.Response(200, json=_list(path, self.deposits))
        if path.startswith("/v1/deposits/"):
            found = [d for d in self.deposits if d["id"] == path.rsplit("/", 1)[1]]
            if not found:
                return _error(404, "resource_missing")
            return httpx.Response(200, json=found[0])
        if path == "/v1/refunds" and request.method == "POST":
            refund = {
                "id": REFUND,
                "object": "refund",
                "livemode": False,
                "deposit": body["deposit"],
                "amount_atomic": body["amount_atomic"],
                "destination_address": body["destination_address"],
                "treasury": CONFIG.treasury,
                "status": "pending",
                "failure_reason": None,
                "transaction_hash": None,
                "receipt_log_index": None,
                "created": NOW,
                "metadata": body.get("metadata", {}),
            }
            self.refunds.append(refund)
            return httpx.Response(200, json=refund)
        if path == "/v1/refunds":
            return httpx.Response(200, json=_list(path, self.refunds))
        if path.endswith("/mark_paid"):
            self.refunds[0].update(transaction_hash=body["transaction_hash"])
            return httpx.Response(200, json=self.refunds[0])
        if path.endswith("/cancel"):
            self.refunds[0].update(status="canceled")
            return httpx.Response(200, json=self.refunds[0])
        if path == "/v1/balance":
            amount = {
                "chain_id": 11155111,
                "token": CONFIG.token,
                "asset": "pha",
                "amount_atomic": "300",
                "final_amount_atomic": "200",
            }
            return httpx.Response(
                200, json={"object": "balance", "livemode": False, "unswept": [amount]}
            )
        if path == "/v1/forwarders":
            return httpx.Response(200, json=_list(path, self.forwarders))
        if path == "/v1/sweeps":
            return httpx.Response(200, json=_list(path, []))
        return _error(404, "not_here")


def _error(status: int, code: str) -> httpx.Response:
    error = {
        "type": "invalid_request_error",
        "code": code,
        "message": code,
        "doc_url": f"https://phala-network.github.io/phala-pay/#section/Errors/{code}",
    }
    return httpx.Response(status, json={"error": error})


def _rpc(request: httpx.Request) -> httpx.Response:
    method = json.loads(request.content)["method"]
    result: Any = {"blockNumber": "0x1"}
    if method == "eth_getBlockByNumber":
        result = {"timestamp": hex(NOW - 12)}
    return httpx.Response(200, json={"jsonrpc": "2.0", "id": 1, "result": result})


@pytest.fixture
def demo(tmp_path: Path) -> tuple[DemoConsole, Service]:
    # The built website's layout (deploy/product/web/dist).
    (tmp_path / "index.html").write_text("<!doctype html><title>Phala Pay</title>")
    (tmp_path / "assets").mkdir()
    (tmp_path / "assets" / "index-0a1b2c.js").write_text("export {};")
    (tmp_path / "secret.txt").write_text("not served")
    (tmp_path / "product.key").write_text("ppay_rk_test_" + "A" * 43 + "000000\n")
    service = Service()
    console = DemoConsole(
        replace(CONFIG, api_key_file=str(tmp_path / "product.key")),
        ProductLedger(),
        tmp_path,
        recorder=ApiRecorder(httpx.MockTransport(service)),
        http=httpx.Client(transport=httpx.MockTransport(_rpc)),
        clock=lambda: NOW,
    )
    return console, service


def _account(console: DemoConsole) -> str:
    response = console.handle("GET", "/api/account", {}, b"")
    assert response.status == HTTPStatus.OK
    cookie = response.headers["set-cookie"]
    assert "Path=/;" in cookie
    assert "HttpOnly" in cookie
    assert "SameSite=Strict" in cookie
    assert "Secure" in cookie
    return cookie.split(";")[0]


def _customer(cookie: str) -> str:
    return cookie.split("=", 1)[1]


def _post(console: DemoConsole, cookie: str, path: str, body: dict[str, Any]) -> Any:
    headers = {"Cookie": cookie, "Content-Type": "application/json"}
    response = console.handle("POST", f"/api/{path}", headers, json.dumps(body).encode())
    return response.status, json.loads(response.body)


def _get(console: DemoConsole, cookie: str, path: str) -> Any:
    response = console.handle("GET", f"/api/{path}", {"Cookie": cookie}, b"")
    return response.status, json.loads(response.body)


def _create_quote(console: DemoConsole, cookie: str) -> dict[str, Any]:
    status, body = _post(console, cookie, "quotes", {"amount": 2500})
    assert status == HTTPStatus.OK
    assert body["client_secret"].startswith(QUOTE)
    # The address the SDK recomputed from the pins, for `<Checkout expectedAddress>`.
    assert re.fullmatch(r"0x[0-9a-fA-F]{40}", body["expected_address"])
    # The console's order id travels in the quote's metadata.
    assert body["api"][0]["request"]["body"]["metadata"]["order_id"] == body["order_id"]
    # The developer view shows only the key's prefix and masks the client secret.
    assert body["api"][0]["request"]["headers"]["authorization"] == "Bearer ppay_rk_test_…"
    assert "AAAA" not in json.dumps(body["api"])
    assert f"{QUOTE}_secret_" not in json.dumps(body["api"])
    return dict(body)


def _steps(console: DemoConsole, cookie: str, path: str = f"quotes/{QUOTE}") -> dict[str, str]:
    status, body = _get(console, cookie, path)
    assert status == HTTPStatus.OK
    return {step["key"]: step["state"] for step in body["steps"]}


def test_an_expired_quote_without_payment_fails_at_the_transfer(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    _create_quote(console, cookie)
    service.quote = {**service.quote, "expires_at": NOW - 1}
    steps = _steps(console, cookie)
    assert steps["sent"] == "failed"
    assert steps["received"] == "upcoming"


def test_a_rejected_deposit_fails_at_the_credit(demo: tuple[DemoConsole, Service]) -> None:
    console, service = demo
    cookie = _account(console)
    _create_quote(console, cookie)
    customer = _customer(cookie)
    service.deposits = [
        _deposit(customer, status="rejected", rejection_reason="sanctioned", amount=None)
    ]
    steps = _steps(console, cookie)
    assert steps["sent"] == "complete"
    assert steps["received"] == "complete"
    assert steps["credited"] == "failed"
    assert steps["webhook_received"] == "upcoming"


def test_a_credited_deposit_waits_for_the_webhook_finality_and_the_sweep(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    _create_quote(console, cookie)
    service.deposits = [_deposit(_customer(cookie), final=False, final_at=None)]
    status, view = _get(console, cookie, f"quotes/{QUOTE}")
    assert status == HTTPStatus.OK
    steps = {step["key"]: step for step in view["steps"]}
    assert steps["credited"]["state"] == "complete"
    assert steps["webhook_received"]["state"] == "current"
    assert steps["final"]["state"] == "current"
    assert steps["swept"]["state"] == "upcoming"
    # Real times: the block's timestamp from the product's RPC, the service's valuation time.
    assert view["sent"]["at"] == NOW - 12
    assert steps["credited"]["at"] == NOW
    service.deposits = [_deposit(_customer(cookie), final=True)]
    steps = {step["key"]: step for step in _get(console, cookie, f"quotes/{QUOTE}")[1]["steps"]}
    assert steps["final"]["state"] == "complete"
    # The deposit's `final_at`, set by the service's finality watch.
    assert steps["final"]["at"] == NOW + 780
    assert steps["swept"]["state"] == "current"


def test_the_ledger_view_applies_the_snapshot_rule(demo: tuple[DemoConsole, Service]) -> None:
    console, service = demo
    cookie = _account(console)
    customer = _customer(cookie)
    service.deposits = [_deposit(customer, amount_refunded=625, amount_refunded_atomic="25")]
    status, view = _get(console, cookie, f"deposits/{DEPOSIT}")
    assert status == HTTPStatus.OK
    assert view["ledger"]["nets_to"] == 1875
    service.deposits = [_deposit(customer, status="reversed", amount_reversed=2500)]
    _, view = _get(console, cookie, f"deposits/{DEPOSIT}")
    assert view["ledger"]["nets_to"] == 0
    assert "reversed" in {step["key"] for step in view["steps"]}
    service.deposits = [_deposit(customer, status="rejected", amount=None)]
    _, view = _get(console, cookie, f"deposits/{DEPOSIT}")
    assert view["ledger"]["nets_to"] == 0


def test_the_deposit_address_is_shown_only_when_the_pins_derive_it(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    status, body = _post(console, cookie, "deposit_address", {})
    assert status == HTTPStatus.OK
    assert body["verified"] is True
    assert body["client_secret"].startswith(ADDRESS_ID)
    assert body["deposit_address"]["networks"][0]["chain_id"] == 11155111
    assert body["deposit_address"]["metadata"] == {"workspace": _customer(cookie)}
    status, body = _get(console, cookie, "deposit_address")
    assert status == HTTPStatus.OK
    assert body["deposit_address"]["id"] == ADDRESS_ID
    # A compromised service naming another address: the SDK refuses it, nothing is shown.
    service.wrong_address = "0x" + "99" * 20
    status, body = _post(console, cookie, "deposit_address", {})
    assert status == HTTPStatus.BAD_GATEWAY
    assert body == {"code": "address_not_derivable"}


def test_refunds_of_own_deposits_show_the_transfer_to_make(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    service.deposits = [_deposit("someone-else")]
    refund = {"deposit": DEPOSIT, "amount_atomic": "40", "destination_address": "0x" + "33" * 20}
    status, _ = _post(console, cookie, "refunds", refund)
    assert status == HTTPStatus.NOT_FOUND
    assert not service.refunds
    service.deposits = [_deposit(_customer(cookie))]
    status, body = _post(console, cookie, "refunds", refund)
    assert status == HTTPStatus.OK
    transfer = body["refund"]["transfer"]
    assert transfer["from"] == CONFIG.treasury
    assert transfer["token"] == "0x" + "44" * 20
    assert transfer["data"] == "0xa9059cbb" + ("33" * 20).rjust(64, "0") + "28".rjust(64, "0")
    for bad in (
        {"transaction_hash": "0x12"},
        {"transaction_hash": "0x" + "ab" * 32, "receipt_log_index": -1},
    ):
        status, body = _post(console, cookie, f"refunds/{REFUND}/mark_paid", bad)
        assert status == HTTPStatus.BAD_REQUEST
    status, body = _post(
        console, cookie, f"refunds/{REFUND}/mark_paid", {"transaction_hash": "0x" + "AB" * 32}
    )
    assert status == HTTPStatus.OK
    assert body["refund"]["transaction_hash"] == "0x" + "ab" * 32
    assert body["refund"]["transfer"] is None
    # Another browser can neither mark nor cancel it.
    other = _account(console)
    status, _ = _post(console, other, f"refunds/{REFUND}/cancel", {})
    assert status == HTTPStatus.NOT_FOUND
    status, body = _post(console, cookie, f"refunds/{REFUND}/cancel", {})
    assert (status, body["refund"]["status"]) == (HTTPStatus.OK, "canceled")


def test_the_sweep_is_built_only_from_forwarders_the_pins_derive(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    salt = quote_salt(ACCOUNT, "acct", QUOTE)
    good = {
        "id": "fwd_" + "01" * 16,
        "object": "forwarder",
        "livemode": False,
        "chain_id": 11155111,
        "address": forwarder_address(CONFIG.factory, CONFIG.implementation, CONFIG.treasury, salt),
        "factory": CONFIG.factory,
        "salt": "0x" + salt.hex(),
        "treasury": CONFIG.treasury,
    }
    # A forwarder over another treasury, or at an address its salt does not give, is refused.
    other_treasury = {**good, "id": "fwd_" + "02" * 16, "treasury": "0x" + "dd" * 20}
    wrong_address = {**good, "id": "fwd_" + "03" * 16, "address": "0x" + "ee" * 20}
    service.forwarders = [good, other_treasury, wrong_address]
    status, view = _get(console, cookie, "sweeps")
    assert status == HTTPStatus.OK
    assert (view["sweepable_forwarders"], view["refused_forwarders"]) == (1, 2)
    assert view["final_unswept_atomic"] == "200"
    [flush] = view["flush"]
    assert flush["to"].lower() == CONFIG.factory
    assert flush["data"].startswith("0x")
    assert salt.hex() in flush["data"]
    assert view["safe_batch"]["meta"]["createdFromSafeAddress"].lower() == CONFIG.treasury
    assert view["safe_batch"]["transactions"] == [flush]


def test_requests_need_the_cookie_and_posts_need_json(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    no_cookie = console.handle("GET", f"/api/quotes/{QUOTE}", {}, b"")
    assert no_cookie.status == HTTPStatus.UNAUTHORIZED
    forged = console.handle("GET", "/api/account", {"Cookie": "demo_account=admin"}, b"")
    assert "set-cookie" in forged.headers
    cookie = _account(console)
    form = {"Cookie": cookie, "Content-Type": "application/x-www-form-urlencoded"}
    for path in ("quotes", "deposit_address", "refunds"):
        post = console.handle("POST", f"/api/{path}", form, b"amount=2500")
        assert post.status == HTTPStatus.UNSUPPORTED_MEDIA_TYPE
    # Another browser's deposit is not found.
    status, _ = _get(console, cookie, f"deposits/{DEPOSIT}")
    assert status == HTTPStatus.NOT_FOUND


def test_serves_the_page_at_the_root(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    assert console.handles("/")
    page = console.handle("GET", "/", {}, b"")
    assert page.status == HTTPStatus.OK
    assert page.body == b"<!doctype html><title>Phala Pay</title>"
    assert page.headers["content-type"] == "text/html; charset=utf-8"
    assert page.headers["cache-control"] == "no-cache"
    assert page.headers["referrer-policy"] == "no-referrer"
    assert page.headers["x-content-type-options"] == "nosniff"
    csp = page.headers["content-security-policy"]
    assert csp.startswith("default-src 'none'; script-src 'self';")
    assert "connect-src 'self' http://service.test;" in csp
    # Its assets, by content hash; nothing else at the root is the website's.
    asset = console.handle("GET", "/assets/index-0a1b2c.js", {}, b"")
    assert asset.status == HTTPStatus.OK
    assert asset.headers["cache-control"] == "public, max-age=31536000"
    assert "content-security-policy" not in asset.headers
    assert console.handle("POST", "/", {}, b"").status == HTTPStatus.METHOD_NOT_ALLOWED
    for path in ["/index.html", "/secret.txt", "/webhooks", "/healthz", "/accounts/x"]:
        assert not console.handles(path)


def test_serves_only_the_built_page(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    for path in [
        "/assets/../secret.txt",
        "/assets/secret.txt",
        "/api/../secret.txt",
        "/api/unknown",
        "/api/assets/index-0a1b2c.js",
    ]:
        assert console.handle("GET", path, {}, b"").status in (
            HTTPStatus.NOT_FOUND,
            HTTPStatus.UNAUTHORIZED,
        )


def test_the_old_demo_paths_are_gone(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    assert console.handles("/api/account")
    assert console.handle("GET", "/api/account", {}, b"").status == HTTPStatus.OK
    # The demo is on the page at `/`: no separate page and no redirect, like any unknown path.
    for path in ["/demo", "/demo/", "/demo/api/account"]:
        assert not console.handles(path)
        assert console.handle("GET", path, {}, b"").status == HTTPStatus.NOT_FOUND


def test_the_config_requires_an_account_id() -> None:
    with pytest.raises(ValueError, match="acct_"):
        replace(CONFIG, account="phala-cloud")
