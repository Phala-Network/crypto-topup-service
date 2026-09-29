"""The Phala Pay demo's API: CORS for the website, account cookies, request policy, the pins, both
collection methods, refunds, sweeps, and the timeline's and ledger's states."""

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
    public_url="https://api.acme.example",
    web_origin="https://acme.example",
)
WEBSITE = {"Origin": "https://acme.example"}


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


def _config_asset(asset: str, chain_id: int, contract: str) -> dict[str, Any]:
    return {
        "asset": asset,
        "chain_id": chain_id,
        "confirmations": "2",
        "contract": contract,
        "decimals": 18,
        "max_deposit_atomic": "1" + "0" * 24,
        "min_amount": 100,
        "min_refund_atomic": "1" + "0" * 18,
        "pricing": "spot",
        "quote_spread_bps": 50,
        "quote_tolerance_bps": 100,
        "quote_ttl_seconds": 900,
        "typical_credit_seconds": 24,
        "typical_finality_seconds": 900,
    }


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
        # Test PHA and a second token on the product's chain, and a token on a chain the
        # product's pins do not cover.
        self.assets = [
            _config_asset("pha", 11155111, CONFIG.token),
            _config_asset("usdc", 11155111, "0x" + "55" * 20),
            _config_asset("pha", 1, "0x" + "66" * 20),
        ]

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        path = request.url.path
        body = json.loads(request.content) if request.content else {}
        if path == "/v1/config":
            config = {
                "object": "config",
                "livemode": False,
                "currency": "usd",
                "assets": self.assets,
                "max_open_amount_per_account": 1_000_000,
                "max_open_amount_per_customer": 500_000,
                "max_open_quotes": 100,
            }
            return httpx.Response(200, json=config)
        if path == "/v1/quotes":
            self.quote = _quote(
                body["client_reference_id"], metadata=body["metadata"], asset=body["asset"]
            )
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
    (tmp_path / "product.key").write_text("ppay_rk_test_" + "A" * 43 + "000000\n")
    service = Service()
    console = DemoConsole(
        replace(CONFIG, api_key_file=str(tmp_path / "product.key")),
        ProductLedger(),
        recorder=ApiRecorder(httpx.MockTransport(service)),
        http=httpx.Client(transport=httpx.MockTransport(_rpc)),
        clock=lambda: NOW,
    )
    return console, service


def _account(console: DemoConsole) -> str:
    response = console.handle("GET", "/api/account", WEBSITE, b"")
    assert response.status == HTTPStatus.OK
    # Host-only (no Domain) on the API's origin, sent with the same-site website's requests.
    cookie = response.headers["set-cookie"]
    name, *attributes = cookie.split("; ")
    assert re.fullmatch(r"demo_account=demo-[0-9a-f]{24}", name)
    assert sorted(attributes) == sorted(
        ["Path=/", f"Max-Age={30 * 86_400}", "HttpOnly", "SameSite=Lax", "Secure"]
    )
    return name


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


def test_offers_the_services_assets_on_the_products_chain(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    # Read before a demo account exists, like the attestation.
    response = console.handle("GET", "/api/assets", WEBSITE, b"")
    assert response.status == HTTPStatus.OK
    assets = json.loads(response.body)["assets"]
    assert [(a["asset"], a["symbol"], a["chain_id"]) for a in assets] == [
        ("pha", "PHA", 11155111),
        ("usdc", "USDC", 11155111),
    ]
    assert assets[0] == {
        "asset": "pha",
        "symbol": "PHA",
        "chain_id": 11155111,
        "network": "Sepolia",
        "testnet": True,
        "contract": CONFIG.token,
        "decimals": 18,
        "pricing": "spot",
        "min_amount": 100,
        "quote_ttl_seconds": 900,
    }
    # Cached: the service's config is read once.
    console.handle("GET", "/api/assets", WEBSITE, b"")
    assert [r.url.path for r in service.requests].count("/v1/config") == 1


def test_a_quote_is_for_an_offered_asset_and_returns_its_locked_rate(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, service = demo
    cookie = _account(console)
    # A token the service takes only on a chain the product's pins do not cover, or none at all.
    for asset in ("dai", 7):
        status, body = _post(console, cookie, "quotes", {"amount": 2500, "asset": asset})
        assert (status, body) == (HTTPStatus.BAD_REQUEST, {"code": "asset_invalid"})
    assert "/v1/quotes" not in [r.url.path for r in service.requests]
    status, body = _post(console, cookie, "quotes", {"amount": 2500, "asset": "usdc"})
    assert status == HTTPStatus.OK
    assert json.loads(service.requests[-1].content)["asset"] == "usdc"
    assert (body["asset"], body["exchange_rate"], body["expires_at"]) == (
        "usdc",
        "25.00000000",
        NOW + 900,
    )
    assert body["amount_atomic"] == "100"
    # The unpaid quote's row carries its asset and rate.
    _, account = _get(console, cookie, "account")
    [row] = account["payments"]
    assert (row["asset"], row["exchange_rate"]) == ("usdc", "25.00000000")
    # Without an asset, the configured token.
    status, body = _post(console, cookie, "quotes", {"amount": 2500})
    assert (status, body["asset"]) == (HTTPStatus.OK, "pha")


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


def test_the_website_origin_reads_the_api_with_credentials(
    demo: tuple[DemoConsole, Service],
) -> None:
    console, _ = demo
    cors = {
        "access-control-allow-origin": "https://acme.example",
        "access-control-allow-credentials": "true",
        "vary": "Origin",
    }
    cookie = _account(console)
    ok = console.handle("GET", "/api/account", {**WEBSITE, "Cookie": cookie}, b"")
    assert ok.status == HTTPStatus.OK
    assert {key: ok.headers[key] for key in cors} == cors
    # Errors too, so the page can read their codes.
    for response in (
        console.handle("GET", f"/api/quotes/{QUOTE}", WEBSITE, b""),
        console.handle("GET", "/api/unknown", {**WEBSITE, "Cookie": cookie}, b""),
        console.handle("POST", "/api/quotes", {**WEBSITE, "Cookie": cookie}, b"amount=1"),
    ):
        assert response.status >= HTTPStatus.BAD_REQUEST
        assert {key: response.headers[key] for key in cors} == cors


def test_answers_the_preflight_of_the_website_only(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    request = {
        **WEBSITE,
        "Access-Control-Request-Method": "POST",
        "Access-Control-Request-Headers": "content-type",
    }
    preflight = console.handle("OPTIONS", "/api/quotes", request, b"")
    assert preflight.status == HTTPStatus.NO_CONTENT
    assert preflight.body == b""
    assert preflight.headers == {
        "access-control-allow-origin": "https://acme.example",
        "access-control-allow-credentials": "true",
        "access-control-allow-methods": "GET, POST",
        "access-control-allow-headers": "content-type",
        "access-control-max-age": "600",
        "vary": "Origin",
    }
    for origin in (
        "https://evil.example",
        "http://acme.example",
        "https://acme.example.evil",
        "null",
    ):
        other = console.handle("OPTIONS", "/api/quotes", {**request, "Origin": origin}, b"")
        assert other.headers == {"vary": "Origin"}


def test_other_origins_get_no_cors_headers(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    for headers in ({"Origin": "https://evil.example"}, {"Origin": "null"}, {}):
        response = console.handle("GET", "/api/account", headers, b"")
        assert not any(key.startswith("access-control-") for key in response.headers)
        assert response.headers["vary"] == "Origin"
    assert console.cors("https://evil.example") == {"vary": "Origin"}


def test_serves_only_the_api(demo: tuple[DemoConsole, Service]) -> None:
    console, _ = demo
    assert console.handles("/api/account")
    # The website is on Cloudflare: the API's origin serves no page and no asset.
    for path in ["/", "/index.html", "/assets/index-0a1b2c.js", "/demo/", "/webhooks", "/healthz"]:
        assert not console.handles(path)
        assert console.handle("GET", path, WEBSITE, b"").status == HTTPStatus.NOT_FOUND
    for path in ["/api/../index.html", "/api/unknown", "/api/assets/index-0a1b2c.js"]:
        assert console.handle("GET", path, {}, b"").status in (
            HTTPStatus.NOT_FOUND,
            HTTPStatus.UNAUTHORIZED,
        )


def test_the_config_requires_the_website_origin() -> None:
    for origin in (
        "https://pay.phala.com/",
        "https://pay.phala.com/app",
        "http://pay.phala.com",
        "https://Pay.phala.com",
        "https://*.phala.com",
        "*",
    ):
        with pytest.raises(ValueError, match="web_origin"):
            replace(CONFIG, web_origin=origin)
    for origin in ("https://pay.phala.com", "https://pay.phala.com:8443", "http://127.0.0.1:4173"):
        assert replace(CONFIG, web_origin=origin).web_origin == origin
    with pytest.raises(ValueError, match="web_origin"):
        DemoConsole(replace(CONFIG, web_origin=None), ProductLedger())


def test_the_config_requires_an_account_id() -> None:
    with pytest.raises(ValueError, match="acct_"):
        replace(CONFIG, account="phala-cloud")


def test_a_ledger_from_before_quotes_kept_their_asset_gains_the_column(tmp_path: Path) -> None:
    ledger = ProductLedger()
    with ledger.transaction() as db:
        db.execute(
            "CREATE TABLE demo_quotes (id TEXT PRIMARY KEY, account TEXT NOT NULL, "
            "amount INTEGER NOT NULL, amount_atomic TEXT NOT NULL, exchange_rate TEXT NOT NULL, "
            "address TEXT NOT NULL, expires_at INTEGER NOT NULL, created INTEGER NOT NULL, "
            "api TEXT NOT NULL)"
        )
    (tmp_path / "product.key").write_text("ppay_rk_test_" + "A" * 43 + "000000\n")
    for _ in range(2):
        DemoConsole(replace(CONFIG, api_key_file=str(tmp_path / "product.key")), ledger)
    with ledger.transaction() as db:
        columns = [row[1] for row in db.execute("PRAGMA table_info(demo_quotes)")]
    assert columns[-1] == "asset"
