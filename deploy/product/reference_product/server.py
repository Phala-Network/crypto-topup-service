"""The product service: webhook receiver with fulfillment, and the product's account API.

It pins the service's settlement key from attestation, keeps its ledger in SQLite, and serves its
own account API, through which a user registers a workspace, gets a quote-first single-use
address, and reads its deposits, credits, and webhook events. It holds the product key and calls
the service on the user's behalf, as Phala Cloud's backend does.
"""

from __future__ import annotations

import json
import logging
import re
import secrets
import signal
import threading
import time
from collections.abc import Iterator, Mapping
from contextlib import contextmanager
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlsplit

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import AttestationResponse, Quote
from topup_sdk import (
    ApiError,
    AttestationError,
    SignatureError,
    TopupClient,
    load_public_key,
    verify_attestation_binding,
    verify_request,
)
from topup_sdk.addresses import forwarder_address, lock_salt, persistent_salt, same_address
from topup_sdk.ids import DEPOSIT, object_id, parse_id

from .config import (
    DRIVER_KEYID,
    EVM_ADDRESS,
    SETTLEMENT_KEYID,
    MissingProductKeyError,
    ProductConfig,
)
from .fulfillment import Answer, Fulfillment, TransientError, parse_decimal
from .ledger import ProductLedger

LOG = logging.getLogger(__name__)

MAX_BODY_BYTES = 1024 * 1024
# Workspace ids and lock references in the account API: URL path segments without escaping.
ACCOUNT_REF = re.compile(r"[A-Za-z0-9._-]{1,64}")


class ProductServer:
    """Serves `POST /webhooks`, `GET /healthz`, and, given an `AccountApi`, `/accounts`."""

    def __init__(self, fulfillment: Fulfillment, accounts: AccountApi | None = None) -> None:
        self.fulfillment = fulfillment
        self.accounts = accounts
        config = fulfillment.config
        base_path = urlsplit(config.public_url).path.rstrip("/")
        server = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                body = self._body()
                if body is None:
                    return
                headers = dict(self.headers.items())
                if self.path == base_path + "/webhooks":
                    self._send(server.fulfillment.handle(headers, body))
                elif server.accounts is not None and server.accounts.handles(self.path):
                    self._send(server.accounts.handle("POST", self.path, headers, body))
                else:
                    self._send(Answer(HTTPStatus.NOT_FOUND))

            def do_GET(self) -> None:
                headers = dict(self.headers.items())
                if self.path == base_path + "/healthz":
                    self._send(Answer(HTTPStatus.OK, {"status": "ok"}))
                elif server.accounts is not None and server.accounts.handles(self.path):
                    self._send(server.accounts.handle("GET", self.path, headers, b""))
                else:
                    self._send(Answer(HTTPStatus.NOT_FOUND))

            def _body(self) -> bytes | None:
                length = int(self.headers.get("content-length") or 0)
                if length > MAX_BODY_BYTES:
                    self._send(Answer(HTTPStatus.REQUEST_ENTITY_TOO_LARGE))
                    return None
                return self.rfile.read(length)

            def _send(self, answer: Answer) -> None:
                payload = b"" if answer.body is None else json.dumps(answer.body).encode()
                self.send_response(answer.status)
                if answer.body is not None:
                    self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, format: str, *args: Any) -> None:
                LOG.debug("%s %s", self.address_string(), format % args)

        self._httpd = ThreadingHTTPServer((config.listen_host, config.listen_port), Handler)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)

    def __enter__(self) -> ProductServer:
        self._thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self._httpd.shutdown()
        self._httpd.server_close()
        self._thread.join()


class AccountApi:
    """The product's account API; the deposit driver uses it as a signed-in user would.

    - `POST /accounts` `{"account_id"}` registers a workspace (`register_team`);
    - `POST /accounts/{id}/quotes` `{"amount_minor"}` creates a quote (`create_quote`) and returns
      the service's quote;
    - `POST /accounts/{id}/deposits/{deposit_id}/refunds` `{"destination_address",
      "amount_atomic"}` requests a refund of one of the workspace's deposits (`create_refund`);
    - `GET /accounts/{id}` returns the workspace's deposits (from the service), its credits
      (from the ledger), and the verified webhook events for those deposits and its quotes.

    The product calls the service with its own key on the user's behalf, as Phala Cloud's
    backend does. Requests must carry an RFC 9421 signature by the pinned driver key
    (`driver_public_key`, key id `driver/v1`), which stands in for user sessions and cannot sign
    service requests. Replays are bounded only by the five-minute freshness window; every
    operation is idempotent.
    """

    def __init__(self, config: ProductConfig, ledger: ProductLedger, driver_key: Ed25519PublicKey):
        self.config = config
        self.ledger = ledger
        self.driver_key = driver_key
        self.accounts_path = urlsplit(config.public_url).path.rstrip("/") + "/accounts"
        self._client: TopupClient | None = None
        self._client_lock = threading.Lock()

    def handles(self, target: str) -> bool:
        path = urlsplit(target).path
        return path == self.accounts_path or path.startswith(self.accounts_path + "/")

    def handle(self, method: str, target: str, headers: Mapping[str, str], body: bytes) -> Answer:
        public = urlsplit(self.config.public_url)
        try:
            verify_request(
                method=method,
                target_uri=f"{public.scheme}://{public.netloc}{target}",
                headers=headers,
                body=body,
                public_key=self.driver_key,
                keyid=DRIVER_KEYID,
                require_idempotency_key=False,
            )
        except SignatureError:
            return Answer(HTTPStatus.UNAUTHORIZED)
        parts = urlsplit(target).path.removeprefix(self.accounts_path).split("/")[1:]
        try:
            if method == "POST" and not parts:
                team = _account_ref(_json_object(body).get("account_id"))
                address = register_team(self.config, self._service(), self.ledger, team)
                return Answer(HTTPStatus.OK, {"account_id": team, "address": address})
            if len(parts) == 2 and parts[1] == "quotes" and method == "POST":
                team = _account_ref(parts[0])
                request = _json_object(body)
                amount_minor = request.get("amount_minor")
                if type(amount_minor) is not int or amount_minor <= 0:
                    raise ValueError("amount_minor must be a positive integer")
                if self.ledger.team_suspended(team) is None:
                    return Answer(HTTPStatus.NOT_FOUND)
                quote = create_quote(
                    self.config, self._service(), self.ledger, team, amount_minor=amount_minor
                )
                return Answer(HTTPStatus.OK, quote.to_dict())
            if (
                len(parts) == 4
                and parts[1] == "deposits"
                and parts[3] == "refunds"
                and method == "POST"
            ):
                team = _account_ref(parts[0])
                deposit = object_id(DEPOSIT, parse_id(DEPOSIT, parts[2]))
                request = _json_object(body)
                destination = request.get("destination_address")
                amount = parse_decimal(request.get("amount_atomic"))
                if not isinstance(destination, str) or not EVM_ADDRESS.fullmatch(destination):
                    raise ValueError("destination_address must be a 0x-prefixed 20-byte address")
                if amount is None or amount <= 0:
                    raise ValueError("amount_atomic must be a positive decimal string")
                if self.ledger.team_suspended(team) is None or not any(
                    item.id == deposit for item in self._service().list_deposits(account_id=team)
                ):
                    return Answer(HTTPStatus.NOT_FOUND)
                refund = self._service().create_refund(deposit, destination, amount)
                return Answer(HTTPStatus.OK, refund.to_dict())
            if len(parts) == 1 and method == "GET":
                team = _account_ref(parts[0])
                if self.ledger.team_suspended(team) is None:
                    return Answer(HTTPStatus.NOT_FOUND)
                return Answer(HTTPStatus.OK, self._account_view(team))
        except ValueError:
            return Answer(HTTPStatus.BAD_REQUEST)
        except MissingProductKeyError:
            LOG.warning("account API unavailable: the product key is not configured")
            return Answer(HTTPStatus.SERVICE_UNAVAILABLE)
        except ApiError as error:
            # The service's documented error code is public; nothing else is passed on.
            LOG.warning("service answered %s %s", error.status_code, error.code)
            return Answer(
                HTTPStatus.BAD_GATEWAY,
                {"service_status": error.status_code, "service_code": error.code},
            )
        except httpx.HTTPError:
            LOG.warning("service unavailable for the account API")
            return Answer(HTTPStatus.SERVICE_UNAVAILABLE)
        except RuntimeError:
            LOG.exception("account API request failed")
            return Answer(HTTPStatus.INTERNAL_SERVER_ERROR)
        return Answer(HTTPStatus.NOT_FOUND)

    def _account_view(self, team: str) -> dict[str, Any]:
        deposits = list(self._service().list_deposits(account_id=team))
        # Webhook events name deposits by their UUID.
        ids = {str(parse_id(DEPOSIT, deposit.id)) for deposit in deposits}
        return {
            "account_id": team,
            "deposits": [deposit.to_dict() for deposit in deposits],
            "credits": [
                {"provider_order_id": key, "amount_minor": amount}
                for key, amount in self.ledger.credits_for(team)
            ],
            "orders": self.ledger.orders_for(team),
            "events": [
                event
                for event in self.ledger.all_events()
                if event["data"].get("deposit_id") in ids
                or event["data"].get("external_id") == team
            ],
        }

    def _service(self) -> TopupClient:
        with self._client_lock:
            if self._client is None:
                self._client = self.config.client()
            return self._client

    def close(self) -> None:
        with self._client_lock:
            if self._client is not None:
                self._client.close()


def _account_ref(value: object) -> str:
    if not isinstance(value, str) or not ACCOUNT_REF.fullmatch(value):
        raise ValueError("expected 1-64 letters, digits, '.', '_', or '-'")
    return value


def _json_object(body: bytes) -> dict[str, Any]:
    value = json.loads(body)
    if not isinstance(value, dict):
        raise ValueError("expected a JSON object")
    return value


def pin_settlement_key(config: ProductConfig, *, wait_s: float = 0) -> Ed25519PublicKey:
    """Returns the settlement key from attestation evidence bound to a fresh nonce.

    `verify_attestation_binding` checks that the report data binds the nonce, the key, and the
    flusher operators. Production integrators must also verify the TDX quote with the dstack
    verifier (deploy/dstack-verifier.sh, deploy/README.md) and then pin `(keyid, public key)` in
    configuration; a configured `settlement_public_key` skips the fetch. While the service is
    unreachable this retries for up to `wait_s` seconds.
    """
    if config.settlement_public_key is not None:
        return load_public_key(config.settlement_public_key)
    deadline = time.monotonic() + wait_s
    while True:
        nonce = secrets.token_bytes(32)
        try:
            response = httpx.get(
                config.service_url.rstrip("/") + "/v1/attestation",
                params={"nonce": nonce.hex()},
                timeout=30,
            )
            response.raise_for_status()
            evidence = AttestationResponse.from_dict(response.json())
            break
        except (httpx.HTTPError, ValueError, KeyError, TypeError) as error:
            if time.monotonic() >= deadline:
                raise TransientError("the service's attestation is unavailable") from error
            LOG.warning(
                "waiting for %s/v1/attestation: %s", config.service_url, type(error).__name__
            )
            time.sleep(5)
    verify_attestation_binding(evidence, nonce)
    if evidence.keyid != SETTLEMENT_KEYID:
        raise AttestationError("attestation names an unexpected settlement key id")
    LOG.warning("pinned settlement key from attestation; verify the quote before production")
    return load_public_key(evidence.settlement_pubkey)


def register_team(
    config: ProductConfig,
    client: TopupClient,
    ledger: ProductLedger,
    team: str,
    *,
    suspended: bool = False,
) -> str:
    """Registers a workspace and records its persistent address after recomputing it.

    The service creates the account with the address, as it does with a first quote.
    """
    ledger.add_team(team, suspended=suspended)
    address = client.create_deposit_address(team)
    inputs = address.salt_inputs
    salt = persistent_salt(inputs.product_slug, inputs.external_id, inputs.version)
    expected = forwarder_address(config.factory, config.implementation, salt)
    if inputs.product_slug != config.product_slug or inputs.external_id != team:
        raise RuntimeError("address salt inputs name another account")
    if not same_address(expected, address.address) or address.chain_id != config.chain_id:
        raise RuntimeError("service returned an address the product cannot recompute")
    ledger.record_address(address.address, team, version=inputs.version)
    return address.address


def create_quote(
    config: ProductConfig,
    client: TopupClient,
    ledger: ProductLedger,
    team: str,
    *,
    amount_minor: int,
) -> Quote:
    """Creates a quote for the workspace and records its address.

    The client recomputes the address from the pinned forwarder and the quote id, and raises
    before returning an address the product did not derive.
    """
    quote = client.create_quote(
        team, amount_minor, chain_id=config.chain_id, asset=config.token_symbol.lower()
    )
    ledger.record_address(quote.address, team, lock_ref=quote.id)
    return quote


def quote_address(config: ProductConfig, team: str, quote_id: str) -> str:
    return forwarder_address(
        config.factory, config.implementation, lock_salt(config.product_slug, team, quote_id)
    )


@contextmanager
def product_service(config: ProductConfig, *, pin_wait_s: float = 0) -> Iterator[ProductServer]:
    """Runs the product: webhook receiver with fulfillment, and account API."""
    if config.driver_public_key is None:
        raise ValueError("driver_public_key is required to serve the account API")
    settlement_key = pin_settlement_key(config, wait_s=pin_wait_s)
    ledger = ProductLedger(config.ledger_path)
    fulfillment = Fulfillment(config, ledger, settlement_key)
    accounts = AccountApi(config, ledger, load_public_key(config.driver_public_key))
    try:
        with ProductServer(fulfillment, accounts) as server:
            LOG.info("product listening on %s:%s", config.listen_host, config.listen_port)
            yield server
    finally:
        accounts.close()


def serve(config: ProductConfig) -> None:
    """Serves the product until SIGTERM or SIGINT."""
    stop = threading.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda *_: stop.set())
    with product_service(config, pin_wait_s=600):
        stop.wait()
    LOG.info("product stopped")
