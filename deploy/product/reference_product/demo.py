"""The Phala Pay demo: a cloud console's "Billing → Add credits" page, served on staging.

With `demo_dir` configured, the product serves the built page of deploy/product/web at
`{public_url}/demo/` and its JSON API at `{public_url}/demo/api/`:

- `GET api/account`: the visitor's demo account (a random id in a cookie; no other data is kept),
  its balance from this product's ledger, and its top-ups;
- `POST api/quotes` `{"amount"}` (cents): creates a quote with the SDK and returns its
  `client_secret` for `<Checkout>`;
- `GET api/quotes/{id}`: the payment's timeline, built only from real data: the service's quote
  and deposit (signed reads with the product key), the webhook events and ledger rows of this
  product, and the sweep transfer on chain; with the service requests behind it;
- `GET api/trust`: the service's attestation, with the report-data binding checked by the SDK, and
  the app id and compose hash of its TLS evidence.

The browser never holds a key: the product signs every service request itself, and the developer
view shows those requests with their signatures shortened. Balances move only through the
`deposit.credited` webhook (reference_product.fulfillment), exactly as for any other account.
"""

from __future__ import annotations

import json
import logging
import re
import secrets
import threading
import time
import uuid
from collections import deque
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass, field
from http import HTTPStatus
from http.cookies import CookieError, SimpleCookie
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import httpx

from topup_client.models import AttestationResponse, Deposit, Quote, QuotePayment
from topup_sdk import ApiError, AttestationError, TopupClient

from .config import MissingProductKeyError, ProductConfig
from .ledger import ORDER_FLOW_CODE, ProductLedger

LOG = logging.getLogger(__name__)

ACCOUNT_COOKIE = "demo_account"
ACCOUNT_ID = re.compile(r"demo-[0-9a-f]{24}")
QUOTE_ID = re.compile(r"qt_[0-9a-f]{32}")
PRESETS = [500, 2000, 5000]
MIN_AMOUNT = 100
MAX_AMOUNT = 100_000
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
EXPLORERS = {1: "https://etherscan.io", 11155111: "https://sepolia.etherscan.io"}
NETWORKS = {1: "Ethereum", 11155111: "Sepolia"}
VERIFY_DOCS = (
    "https://github.com/Phala-Network/phala-pay/blob/main/deploy/README.md"
    "#attestation-ingress-and-egress"
)
CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".svg": "image/svg+xml",
}
SCHEMA = """
CREATE TABLE IF NOT EXISTS demo_quotes (
    id TEXT PRIMARY KEY,
    account TEXT NOT NULL REFERENCES teams (id),
    amount INTEGER NOT NULL,
    amount_atomic TEXT NOT NULL,
    exchange_rate TEXT NOT NULL,
    address TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created INTEGER NOT NULL,
    api TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS demo_quotes_account ON demo_quotes (account, created);
"""


@dataclass(frozen=True)
class Response:
    status: HTTPStatus
    body: bytes = b""
    headers: dict[str, str] = field(default_factory=dict)


class RateLimiter:
    """At most `limit` events per key in any `window` seconds."""

    def __init__(self, limit: int, window: float, clock: Callable[[], float]) -> None:
        self.limit = limit
        self.window = window
        self._clock = clock
        self._hits: dict[str, deque[float]] = {}
        self._lock = threading.Lock()

    def allow(self, key: str) -> bool:
        now = self._clock()
        with self._lock:
            hits = self._hits.setdefault(key, deque())
            while hits and hits[0] <= now - self.window:
                hits.popleft()
            if len(hits) >= self.limit:
                return False
            hits.append(now)
            if len(self._hits) > 10_000:
                self._hits = {
                    k: v for k, v in self._hits.items() if v and v[-1] > now - self.window
                }
            return True


class ApiRecorder(httpx.BaseTransport):
    """The product's transport to the service; records the exchanges of the current request for
    the developer view, with the RFC 9421 signature shortened and client secrets masked."""

    def __init__(self, inner: httpx.BaseTransport | None = None) -> None:
        self._inner = inner or httpx.HTTPTransport()
        self._local = threading.local()

    @contextmanager
    def capture(self) -> Iterator[list[dict[str, Any]]]:
        calls: list[dict[str, Any]] = []
        self._local.calls = calls
        try:
            yield calls
        finally:
            self._local.calls = None

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        response = self._inner.handle_request(request)
        calls: list[dict[str, Any]] | None = getattr(self._local, "calls", None)
        if calls is not None:
            response.read()
            calls.append(_exchange(request, response))
        return response

    def close(self) -> None:
        self._inner.close()


class DemoConsole:
    """Serves the demo page and its API; see the module docstring."""

    def __init__(
        self,
        config: ProductConfig,
        ledger: ProductLedger,
        demo_dir: str | Path,
        *,
        recorder: ApiRecorder | None = None,
        http: httpx.Client | None = None,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self.config = config
        self.ledger = ledger
        self.base = urlsplit(config.public_url).path.rstrip("/") + "/demo"
        root = Path(demo_dir)
        # Only files present at startup are served, by exact name: no request path is resolved.
        self.files = {
            file.relative_to(root).as_posix(): file.read_bytes()
            for file in root.rglob("*")
            if file.is_file() and file.suffix in CONTENT_TYPES
        }
        if "index.html" not in self.files:
            raise ValueError(f"{root} has no built demo page (index.html)")
        service = urlsplit(config.service_url)
        self.csp = (
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
            f"img-src 'self' data:; connect-src 'self' {service.scheme}://{service.netloc}; "
            "base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        )
        self.secure_cookie = urlsplit(config.public_url).scheme == "https"
        self.recorder = recorder or ApiRecorder()
        self._http = http or httpx.Client(timeout=10, follow_redirects=False)
        self._clock = clock
        self._client: TopupClient | None = None
        self._lock = threading.Lock()
        self._new_accounts = RateLimiter(30, 60, clock)
        self._quotes_per_account = RateLimiter(3, 60, clock)
        self._quotes_per_day = RateLimiter(20, 86_400, clock)
        self._quotes = RateLimiter(30, 60, clock)
        self._reads = RateLimiter(90, 60, clock)
        self._trust: tuple[float, dict[str, Any]] | None = None
        self._sweeps: dict[str, dict[str, str]] = {}
        with ledger.transaction() as db:
            for statement in SCHEMA.split(";"):
                if statement.strip():
                    db.execute(statement)

    # Routing ------------------------------------------------------------------------------------

    def handles(self, target: str) -> bool:
        path = urlsplit(target).path
        return path == self.base or path.startswith(self.base + "/")

    def handle(self, method: str, target: str, headers: dict[str, str], body: bytes) -> Response:
        path = urlsplit(target).path
        if path == self.base:
            return Response(HTTPStatus.MOVED_PERMANENTLY, headers={"location": self.base + "/"})
        name = path.removeprefix(self.base + "/")
        if not name.startswith("api/"):
            return self._static(method, name)
        lowered = {key.lower(): value for key, value in headers.items()}
        try:
            return self._api(method, name.removeprefix("api/"), lowered, body)
        except ApiError as error:
            # The service's documented code is public; its message and everything else are not.
            LOG.warning("demo: service answered %s %s", error.status_code, error.code)
            status = (
                HTTPStatus.TOO_MANY_REQUESTS if error.status_code == 429 else HTTPStatus.BAD_GATEWAY
            )
            return _json(status, {"code": error.code})
        except (httpx.HTTPError, MissingProductKeyError):
            LOG.warning("demo: service unavailable", exc_info=True)
            return _json(HTTPStatus.SERVICE_UNAVAILABLE, {"code": "unavailable"})

    def _api(self, method: str, name: str, headers: dict[str, str], body: bytes) -> Response:
        if name == "trust" and method == "GET":
            # The same for every visitor, cached, and read before a demo account exists.
            return _json(HTTPStatus.OK, self._trust_view())
        account = _cookie_account(headers.get("cookie", ""))
        if name == "account" and method == "GET":
            cookie = None
            if account is None:
                if not self._new_accounts.allow("global"):
                    return _json(HTTPStatus.TOO_MANY_REQUESTS, {"code": "rate_limited"})
                account = f"demo-{secrets.token_hex(12)}"
                cookie = self._set_cookie(account)
            self._ensure_account(account)
            response = _json(HTTPStatus.OK, self._account(account))
            if cookie is not None:
                response.headers["set-cookie"] = cookie
            return response
        if account is None:
            return _json(HTTPStatus.UNAUTHORIZED, {"code": "no_demo_account"})
        self._ensure_account(account)
        if name == "quotes" and method == "POST":
            # A JSON body forces a CORS preflight, which this API never answers, for other sites.
            if not headers.get("content-type", "").startswith("application/json"):
                return _json(HTTPStatus.UNSUPPORTED_MEDIA_TYPE, {"code": "json_required"})
            return self._create_quote(account, body)
        if method != "GET":
            return _json(HTTPStatus.METHOD_NOT_ALLOWED, {"code": "method_not_allowed"})
        if not self._reads.allow(account):
            return _json(HTTPStatus.TOO_MANY_REQUESTS, {"code": "rate_limited"})
        quote_id = name.removeprefix("quotes/")
        if name.startswith("quotes/") and QUOTE_ID.fullmatch(quote_id):
            view = self._timeline(account, quote_id)
            if view is None:
                return _json(HTTPStatus.NOT_FOUND, {"code": "not_found"})
            return _json(HTTPStatus.OK, view)
        return _json(HTTPStatus.NOT_FOUND, {"code": "not_found"})

    def _static(self, method: str, name: str) -> Response:
        if method != "GET":
            return Response(HTTPStatus.METHOD_NOT_ALLOWED)
        name = name or "index.html"
        content = self.files.get(name)
        if content is None:
            return Response(HTTPStatus.NOT_FOUND)
        headers = {
            "content-type": CONTENT_TYPES[Path(name).suffix],
            "x-content-type-options": "nosniff",
            "cache-control": "no-cache" if name == "index.html" else "public, max-age=31536000",
        }
        if name == "index.html":
            headers["content-security-policy"] = self.csp
            headers["referrer-policy"] = "no-referrer"
        return Response(HTTPStatus.OK, content, headers)

    # Accounts -----------------------------------------------------------------------------------

    def _set_cookie(self, account: str) -> str:
        cookie = (
            f"{ACCOUNT_COOKIE}={account}; Path={self.base}/; Max-Age={30 * 86_400}; "
            "HttpOnly; SameSite=Strict"
        )
        return cookie + ("; Secure" if self.secure_cookie else "")

    def _ensure_account(self, account: str) -> None:
        if self.ledger.team_suspended(account) is None:
            self.ledger.add_team(account)

    def _account(self, account: str) -> dict[str, Any]:
        deposits = {
            deposit.quote: deposit
            for deposit in _take(self._service().list_deposits(account_id=account), 50)
            if isinstance(deposit.quote, str)
        }
        with self.ledger.transaction() as db:
            balance = db.execute(
                "SELECT COALESCE(SUM(amount_minor), 0) FROM credit_transactions WHERE team_id = ?",
                (account,),
            ).fetchone()[0]
            rows = db.execute(
                "SELECT id, amount, amount_atomic, expires_at, created FROM demo_quotes "
                "WHERE account = ? ORDER BY created DESC LIMIT 20",
                (account,),
            ).fetchall()
            credits = {
                quote_id: self._credit(db, deposits[quote_id].id)
                for quote_id, *_ in rows
                if quote_id in deposits
            }
        now = self._clock()
        transactions = []
        for quote_id, amount, amount_atomic, expires_at, created in rows:
            deposit = deposits.get(quote_id)
            credit = credits.get(quote_id)
            if deposit is not None:
                status = deposit.status
            else:
                status = "expired" if now >= expires_at else "awaiting_payment"
            transactions.append(
                {
                    "quote": quote_id,
                    "created": created,
                    "amount": amount,
                    "amount_atomic": amount_atomic,
                    "status": status,
                    "deposit": None if deposit is None else deposit.id,
                    "tx_hash": None if deposit is None else deposit.tx_hash,
                    "paid_atomic": None if deposit is None else deposit.amount_atomic,
                    "credited": None if credit is None else credit["amount"],
                    "refunded_atomic": "0" if deposit is None else deposit.amount_refunded_atomic,
                }
            )
        return {
            "account_id": account,
            "balance": int(balance),
            "presets": PRESETS,
            "min_amount": MIN_AMOUNT,
            "max_amount": MAX_AMOUNT,
            "api_base": self.config.service_url,
            "network": {
                "chain_id": self.config.chain_id,
                "name": NETWORKS.get(self.config.chain_id, f"Chain {self.config.chain_id}"),
                "explorer": EXPLORERS.get(self.config.chain_id),
                "testnet": self.config.chain_id != 1,
            },
            "token": {"symbol": self.config.token_symbol, "address": self.config.token},
            "transactions": transactions,
        }

    # Quotes -------------------------------------------------------------------------------------

    def _create_quote(self, account: str, body: bytes) -> Response:
        try:
            request = json.loads(body)
            amount = request.get("amount") if isinstance(request, dict) else None
        except ValueError:
            amount = None
        if type(amount) is not int or not MIN_AMOUNT <= amount <= MAX_AMOUNT:
            return _json(HTTPStatus.BAD_REQUEST, {"code": "amount_invalid"})
        if not (
            self._quotes.allow("global")
            and self._quotes_per_account.allow(account)
            and self._quotes_per_day.allow(account)
        ):
            return _json(HTTPStatus.TOO_MANY_REQUESTS, {"code": "rate_limited"})
        with self.recorder.capture() as calls:
            quote = self._service().create_quote(
                account,
                amount,
                chain_id=self.config.chain_id,
                asset=self.config.token_symbol.lower(),
                idempotency_key=str(uuid.uuid4()),
            )
        if not isinstance(quote.client_secret, str):
            return _json(HTTPStatus.BAD_GATEWAY, {"code": "unexpected_response"})
        self.ledger.record_quote_address(quote.address, account, quote.id)
        with self.ledger.transaction() as db:
            db.execute(
                "INSERT OR IGNORE INTO demo_quotes (id, account, amount, amount_atomic, "
                "exchange_rate, address, expires_at, created, api) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    quote.id,
                    account,
                    quote.amount,
                    quote.amount_atomic,
                    quote.exchange_rate,
                    quote.address,
                    quote.expires_at,
                    quote.created,
                    json.dumps(calls),
                ),
            )
        return _json(
            HTTPStatus.OK, {"quote": quote.id, "client_secret": quote.client_secret, "api": calls}
        )

    def _timeline(self, account: str, quote_id: str) -> dict[str, Any] | None:
        with self.ledger.transaction() as db:
            row = db.execute(
                "SELECT api FROM demo_quotes WHERE id = ? AND account = ?", (quote_id, account)
            ).fetchone()
        if row is None:
            return None
        with self.recorder.capture() as calls:
            quote = self._service().get_quote(quote_id)
            deposits = list(_take(self._service().list_deposits(quote=quote_id), 10))
        deposit = deposits[0] if deposits else None
        events = self._events(quote, deposit)
        with self.ledger.transaction() as db:
            credit = None if deposit is None else self._credit(db, deposit.id)
        sweep = self._sweep(deposit) if deposit is not None and deposit.status == "swept" else None
        return {
            "quote": _quote_view(quote),
            "deposit": None if deposit is None else deposit.to_dict(),
            "steps": _steps(
                quote,
                deposit=deposit,
                events=events,
                credit=credit,
                sweep=sweep,
                now=self._clock(),
            ),
            "events": events,
            "api": [*json.loads(row[0]), *calls],
        }

    def _events(self, quote: Quote, deposit: Deposit | None) -> list[dict[str, Any]]:
        """This product's verified webhook events about the quote or its deposit."""
        keys = {quote.id} if deposit is None else {quote.id, deposit.id}
        with self.ledger.transaction() as db:
            rows = db.execute(
                "SELECT id, type, data, received_at FROM webhook_events ORDER BY received_at"
            ).fetchall()
        events = []
        for event_id, event_type, data, received_at in rows:
            payload = json.loads(data)
            if not keys & _event_refs(payload):
                continue
            events.append(
                {
                    "id": event_id,
                    "type": event_type,
                    "received_at": received_at,
                    # Only events whose signature verified against the pinned key are stored.
                    "verified": True,
                    "data": payload,
                }
            )
        return events

    def _credit(self, db: Any, deposit_id: str) -> dict[str, Any] | None:
        """The ledger's order and credit for a deposit (its order key is the deposit id)."""
        row = db.execute(
            "SELECT o.provider_order_id, o.status, o.reason, c.id, c.amount_minor, c.created_at "
            "FROM orders o LEFT JOIN credit_transactions c ON c.order_id = o.id "
            "WHERE o.order_flow_code = ? AND o.provider_order_id = ?",
            (ORDER_FLOW_CODE, deposit_id),
        ).fetchone()
        if row is None:
            return None
        return {
            "order_key": row[0],
            "status": row[1],
            "reason": row[2],
            "credit_transaction": row[3],
            "amount": row[4],
            "at": row[5],
        }

    def _sweep(self, deposit: Deposit) -> dict[str, str] | None:
        """The on-chain transfer that swept the deposit's address: the first token transfer out of
        it after the deposit (a forwarder can pay only the treasury)."""
        cached = self._sweeps.get(deposit.id)
        if cached is not None:
            return cached
        try:
            response = self._http.post(
                self.config.rpc_url,
                json={
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getLogs",
                    "params": [
                        {
                            "address": deposit.asset_contract,
                            "fromBlock": hex(deposit.block_number),
                            "toBlock": "latest",
                            "topics": [TRANSFER_TOPIC, _topic(deposit.address)],
                        }
                    ],
                },
            )
            logs = response.json().get("result")
        except (httpx.HTTPError, ValueError):
            LOG.warning("demo: sweep lookup failed", exc_info=True)
            return None
        if not isinstance(logs, list):
            return None
        for log in logs:
            position = (int(log["blockNumber"], 16), int(log["logIndex"], 16))
            if position > (deposit.block_number, deposit.log_index):
                sweep = {"tx_hash": log["transactionHash"], "to": "0x" + log["topics"][2][-40:]}
                self._sweeps[deposit.id] = sweep
                return sweep
        return None

    # Trust --------------------------------------------------------------------------------------

    def _trust_view(self) -> dict[str, Any]:
        now = self._clock()
        with self._lock:
            if self._trust is not None and now - self._trust[0] < 300:
                return self._trust[1]
        attestation: dict[str, Any]
        try:
            evidence = self._service().attestation(secrets.token_bytes(32))
            attestation = _attestation_view(evidence)
        except AttestationError:
            attestation = {"binding_verified": False}
        view = {
            "attestation": attestation,
            "tls_evidence": self._tls_evidence(),
            "verify_docs": VERIFY_DOCS,
            "dstack_verifier": "https://github.com/Dstack-TEE/dstack/tree/master/verifier",
        }
        with self._lock:
            self._trust = (now, view)
        return view

    def _tls_evidence(self) -> dict[str, str] | None:
        """App id, compose hash, and OS image of the dstack-ingress certificate evidence quote
        (RTMR3 events), as deploy/verify-ingress-evidence.sh reads them."""
        url = self.config.service_url.rstrip("/") + "/evidences/quote.json"
        try:
            body = self._http.get(url).raise_for_status().json()
            log = body["event_log"]
            entries = json.loads(log) if isinstance(log, str) else log
        except (httpx.HTTPError, ValueError, KeyError, TypeError):
            return None
        names = {
            "app-id": "app_id",
            "compose-hash": "compose_hash",
            "os-image-hash": "os_image_hash",
        }
        found = {
            names[entry["event"]]: str(entry.get("event_payload", ""))
            for entry in entries
            if isinstance(entry, dict) and entry.get("imr") == 3 and entry.get("event") in names
        }
        return {**found, "url": url} if "app_id" in found else None

    # Service ------------------------------------------------------------------------------------

    def _service(self) -> TopupClient:
        with self._lock:
            if self._client is None:
                self._client = TopupClient(
                    self.config.service_url,
                    self.config.signer(),
                    forwarder=(
                        self.config.factory,
                        self.config.implementation,
                        self.config.treasury,
                    ),
                    transport=self.recorder,
                )
            return self._client

    def close(self) -> None:
        with self._lock:
            if self._client is not None:
                self._client.close()
        self._http.close()


# Views ------------------------------------------------------------------------------------------


def _steps(
    quote: Quote,
    *,
    deposit: Deposit | None,
    events: list[dict[str, Any]],
    credit: dict[str, Any] | None,
    sweep: dict[str, str] | None,
    now: float,
) -> list[dict[str, Any]]:
    """The payment's timeline; every value comes from the service, the ledger, or the chain."""
    payment = quote.payment if isinstance(quote.payment, QuotePayment) else None
    status = None if deposit is None else deposit.status
    final = status in ("confirmed", "credited", "swept")
    credited = status in ("credited", "swept")
    delivered = next((e for e in events if e["type"] == "deposit.credited"), None)
    expired = quote.status in ("expired", "canceled") or (
        quote.status == "open" and now >= quote.expires_at
    )

    def step(
        key: str, state: str, at: float | None, details: list[dict[str, Any]]
    ) -> dict[str, Any]:
        return {"key": key, "state": state, "at": at, "details": details}

    steps = [
        step(
            "quote_created",
            "complete",
            quote.created,
            [
                {"label": "Quote", "value": quote.id, "mono": True},
                {
                    "label": "Locked price",
                    "value": f"{quote.exchange_rate} USD per {quote.asset.upper()}",
                },
                {"label": "Amount to pay", "value": quote.amount_atomic, "kind": "atomic"},
                {"label": "Forwarder address", "value": quote.address, "kind": "address"},
                {"label": "Expires", "value": quote.expires_at, "kind": "time"},
            ],
        )
    ]
    tx_hash = deposit.tx_hash if deposit is not None else (payment.tx_hash if payment else None)
    if deposit is not None:
        details: list[dict[str, Any]] = [
            {"label": "Transaction", "value": deposit.tx_hash, "kind": "tx"},
            {"label": "From", "value": deposit.from_address, "kind": "address"},
        ]
        steps.append(step("transfer_seen", "complete", None, details))
    elif payment is not None:
        details = [{"label": "Transaction", "value": payment.tx_hash, "kind": "tx"}]
        if isinstance(payment.confirmations, int):
            details.append({"label": "Confirmations", "value": payment.confirmations})
        details.append(
            {"label": "Matches the quote", "value": "yes" if payment.matches_quote else "no"}
        )
        steps.append(step("transfer_seen", "complete", None, details))
    else:
        steps.append(step("transfer_seen", "failed" if expired else "current", None, []))

    if final and deposit is not None:
        steps.append(
            step(
                "finalized",
                "complete",
                deposit.created,
                [
                    {"label": "Block", "value": deposit.block_number},
                    {"label": "Log index", "value": deposit.log_index},
                    {"label": "Deposit", "value": deposit.id, "mono": True},
                ],
            )
        )
    else:
        waiting = tx_hash is not None and status != "rejected"
        steps.append(step("finalized", "current" if waiting else "upcoming", None, []))

    if status == "rejected" and deposit is not None:
        reason = deposit.rejection_reason if isinstance(deposit.rejection_reason, str) else ""
        steps.append(step("credited", "failed", None, [{"label": "Reason", "value": reason}]))
    elif credited and deposit is not None:
        amount = deposit.amount if isinstance(deposit.amount, int) else None
        steps.append(
            step(
                "credited",
                "complete",
                deposit.valued_at if isinstance(deposit.valued_at, int) else None,
                [
                    {"label": "Credit", "value": amount, "kind": "usd"},
                    {"label": "Priced at", "value": str(deposit.price_source)},
                    {"label": "Rate", "value": str(deposit.exchange_rate)},
                ],
            )
        )
    else:
        steps.append(step("credited", "current" if final else "upcoming", None, []))

    if delivered is not None:
        details = [
            {"label": "Event", "value": delivered["id"], "mono": True},
            {"label": "Signature", "value": "verified (Standard Webhooks v1a, pinned key)"},
        ]
        if credit is not None:
            details += [
                {"label": "Ledger order", "value": f"{credit['status']} ({credit['order_key']})"},
                {
                    "label": "Credit transaction",
                    "value": credit["credit_transaction"],
                    "mono": True,
                },
                {"label": "Balance", "value": credit["amount"], "kind": "usd_delta"},
            ]
        steps.append(step("webhook_received", "complete", delivered["received_at"], details))
    else:
        steps.append(step("webhook_received", "current" if credited else "upcoming", None, []))

    if status == "swept":
        details = []
        if sweep is not None:
            details = [
                {"label": "Sweep transaction", "value": sweep["tx_hash"], "kind": "tx"},
                {"label": "Treasury", "value": sweep["to"], "kind": "address"},
            ]
        steps.append(step("swept", "complete", None, details))
    else:
        steps.append(step("swept", "current" if credited else "upcoming", None, []))
    return steps


def _quote_view(quote: Quote) -> dict[str, Any]:
    return {
        "id": quote.id,
        "status": quote.status,
        "amount": quote.amount,
        "amount_atomic": quote.amount_atomic,
        "exchange_rate": quote.exchange_rate,
        "address": quote.address,
        "expires_at": quote.expires_at,
        "created": quote.created,
    }


def _attestation_view(evidence: AttestationResponse) -> dict[str, Any]:
    operators = evidence.operators if isinstance(evidence.operators, list) else []
    return {
        # TopupClient.attestation raises unless report_data binds the fresh nonce, the settlement
        # key, and every operator.
        "binding_verified": True,
        "keyid": evidence.keyid,
        "settlement_pubkey": evidence.settlement_pubkey,
        "report_data": evidence.report_data,
        "quote_bytes": len(evidence.quote) // 2,
        "operators": [
            {"chain_id": operator.chain_id, "address": operator.address} for operator in operators
        ],
    }


def _event_refs(payload: dict[str, Any]) -> set[str]:
    """Deposit and quote ids an event names, in the flat and the enveloped (`object`) forms."""
    inner = payload.get("object")
    sources = [payload, inner] if isinstance(inner, dict) else [payload]
    return {
        value
        for source in sources
        for key in ("deposit_id", "id", "quote", "deposit")
        if isinstance(value := source.get(key), str)
    }


def _exchange(request: httpx.Request, response: httpx.Response) -> dict[str, Any]:
    headers = {
        name: _shorten(value) if name == "signature" else value
        for name, value in request.headers.items()
        if name
        in ("content-type", "idempotency-key", "content-digest", "signature-input", "signature")
    }
    return {
        "method": request.method,
        "url": str(request.url),
        "request": {"headers": headers, "body": _body(request.content)},
        "status": response.status_code,
        "response": _mask(_body(response.content)),
    }


def _body(content: bytes) -> Any:
    if not content:
        return None
    try:
        return json.loads(content)
    except ValueError:
        return "<non-JSON body>"


def _mask(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: "qt_…_secret_… (handed to this browser's checkout)"
            if key == "client_secret" and isinstance(item, str)
            else _mask(item)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [_mask(item) for item in value]
    return value


def _shorten(value: str) -> str:
    return value if len(value) <= 40 else value[:24] + "…" + value[-8:]


def _topic(address: str) -> str:
    return "0x" + "0" * 24 + address.lower().removeprefix("0x")


def _take[T](items: Iterator[T], limit: int) -> Iterator[T]:
    for index, item in enumerate(items):
        if index >= limit:
            return
        yield item


def _cookie_account(header: str) -> str | None:
    try:
        cookie = SimpleCookie(header)
    except CookieError:
        return None
    morsel = cookie.get(ACCOUNT_COOKIE)
    if morsel is None or not ACCOUNT_ID.fullmatch(morsel.value):
        return None
    return morsel.value


def _json(status: HTTPStatus, body: dict[str, Any]) -> Response:
    return Response(
        status,
        json.dumps(body).encode(),
        {"content-type": "application/json", "cache-control": "no-store"},
    )
