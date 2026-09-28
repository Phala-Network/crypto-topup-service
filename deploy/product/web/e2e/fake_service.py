"""A stand-in for the Phala Pay service in the demo's end-to-end test.

It answers the product API the demo uses (quotes, the public quote view, deposits, attestation,
the TLS evidence), and follows real payments on Anvil the way the service does, compressed in
time: a transfer to a quote's address is `seen`, then final after three blocks (`confirmed`),
then `credited` with a signed `deposit.credited` webhook to the product, then swept to the
treasury by an actual token transfer out of the forwarder address. Addresses, deposit ids, the
webhook signature, and the attestation binding use the SDK's own helpers, so the product checks
them exactly as it checks the real service.

    python fake_service.py --port 8545 --rpc http://127.0.0.1:8546 --token 0x… \\
        --product-webhook http://127.0.0.1:8089/webhooks --settlement-seed <64 hex> \\
        --factory 0x… --implementation 0x… --product acme --treasury 0x…
"""

from __future__ import annotations

import argparse
import json
import logging
import secrets
import threading
import time
import uuid
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import parse_qs, urlsplit

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from topup_sdk import attestation_report_data, credited_event_id, sign_webhook
from topup_sdk.addresses import deposit_id, forwarder_address, lock_salt

LOG = logging.getLogger("fake_service")
TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
CHAIN_ID = 11155111
PRICE = "0.25000000"  # USD per token
FINAL_AFTER = 3


@dataclass
class State:
    quotes: dict[str, dict[str, Any]] = field(default_factory=dict)
    secrets: dict[str, str] = field(default_factory=dict)
    deposits: dict[str, dict[str, Any]] = field(default_factory=dict)
    lock: threading.Lock = field(default_factory=threading.Lock)


class FakeTopup:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.state = State()
        self.key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(args.settlement_seed))
        self.rpc = httpx.Client(timeout=10)
        self.stop = threading.Event()

    # Chain ------------------------------------------------------------------------------------

    def call(self, method: str, *params: Any) -> Any:
        body = self.rpc.post(
            self.args.rpc, json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).json()
        if "error" in body:
            raise RuntimeError(f"{method}: {body['error']}")
        return body["result"]

    def watch(self) -> None:
        while not self.stop.wait(0.5):
            try:
                self.call("evm_mine")
                self.step()
            except (httpx.HTTPError, RuntimeError):
                LOG.exception("watch step failed")

    def step(self) -> None:
        head = int(self.call("eth_blockNumber"), 16)
        with self.state.lock:
            quotes = [q for q in self.state.quotes.values() if q["payment"] is None]
            deposits = list(self.state.deposits.values())
        for quote in quotes:
            logs = self.call(
                "eth_getLogs",
                {
                    "address": self.args.token,
                    "fromBlock": "0x0",
                    "toBlock": "latest",
                    "topics": [
                        TRANSFER_TOPIC,
                        None,
                        "0x" + "0" * 24 + quote["address"][2:].lower(),
                    ],
                },
            )
            if logs:
                log = logs[0]
                with self.state.lock:
                    quote["payment"] = {
                        "tx_hash": log["transactionHash"],
                        "block": int(log["blockNumber"], 16),
                        "log_index": int(log["logIndex"], 16),
                        "from": "0x" + log["topics"][1][-40:],
                        "amount_atomic": str(int(log["data"], 16)),
                    }
        for quote in list(self.state.quotes.values()):
            payment = quote["payment"]
            if payment is None or quote["deposit"] is not None:
                continue
            if head - payment["block"] + 1 >= FINAL_AFTER:
                self.confirm(quote, payment)
        for deposit in deposits:
            if deposit["status"] == "confirmed":
                self.credit(deposit)
            elif deposit["status"] == "credited" and not deposit["_acknowledged"]:
                self.deliver(deposit)
            elif deposit["status"] == "credited":
                self.sweep(deposit)

    def confirm(self, quote: dict[str, Any], payment: dict[str, Any]) -> None:
        deposit = {
            "id": deposit_id(CHAIN_ID, payment["tx_hash"], payment["log_index"]),
            "object": "deposit",
            "account_id": quote["account_id"],
            "quote": quote["id"],
            "status": "confirmed",
            "rejection_reason": None,
            "chain_id": CHAIN_ID,
            "asset": "pha",
            "asset_contract": self.args.token.lower(),
            "amount_atomic": payment["amount_atomic"],
            "amount": None,
            "currency": "usd",
            "exchange_rate": None,
            "price_source": None,
            "valued_at": None,
            "address": quote["address"],
            "from_address": payment["from"],
            "tx_hash": payment["tx_hash"],
            "log_index": payment["log_index"],
            "block_number": payment["block"],
            "amount_refunded_atomic": "0",
            "refunded": False,
            "created": int(time.time()),
            "_acknowledged": False,
        }
        with self.state.lock:
            quote["deposit"] = deposit["id"]
            quote["status"] = "complete"
            self.state.deposits[deposit["id"]] = deposit

    def credit(self, deposit: dict[str, Any]) -> None:
        quote = self.state.quotes[deposit["quote"]]
        with self.state.lock:
            deposit.update(
                status="credited",
                amount=quote["amount"],
                exchange_rate=PRICE,
                price_source="quote",
                valued_at=int(time.time()),
            )
        self.deliver(deposit)

    def deliver(self, deposit: dict[str, Any]) -> None:
        """Sends `deposit.credited`, as the outbox does, until the product answers `2xx`."""
        event_id = credited_event_id(deposit["id"])
        body = json.dumps(
            {
                "id": event_id,
                "object": "event",
                "type": "deposit.credited",
                "created": int(time.time()),
                "data": {"object": {k: v for k, v in deposit.items() if not k.startswith("_")}},
            }
        ).encode()
        headers = sign_webhook(self.key, event_id, int(time.time()), body)
        try:
            response = httpx.post(
                self.args.product_webhook,
                content=body,
                headers={**headers, "content-type": "application/json"},
                timeout=10,
            )
        except httpx.HTTPError:
            LOG.warning("webhook delivery failed; retrying")
            return
        with self.state.lock:
            deposit["_acknowledged"] = response.is_success

    def sweep(self, deposit: dict[str, Any]) -> None:
        forwarder = deposit["address"]
        self.call("anvil_impersonateAccount", forwarder)
        self.call("anvil_setBalance", forwarder, hex(10**18))
        amount = int(deposit["amount_atomic"])
        data = (
            "0xa9059cbb"
            + self.args.treasury[2:].lower().rjust(64, "0")
            + format(amount, "x").rjust(64, "0")
        )
        tx = self.call(
            "eth_sendTransaction", {"from": forwarder, "to": self.args.token, "data": data}
        )
        self.call("anvil_stopImpersonatingAccount", forwarder)
        with self.state.lock:
            deposit["status"] = "swept"
        LOG.info("swept %s in %s", deposit["id"], tx)

    # API --------------------------------------------------------------------------------------

    def create_quote(self, body: dict[str, Any], idempotency_key: str) -> dict[str, Any]:
        quote_id = "qt_" + uuid.uuid5(uuid.NAMESPACE_OID, idempotency_key).hex
        account = str(body["account_id"])
        amount = int(body["amount"])
        address = forwarder_address(
            self.args.factory,
            self.args.implementation,
            self.args.treasury,
            lock_salt(self.args.product, account, quote_id),
        )
        atomic = str(amount * 4 * 10**16)  # cents / 100 / 0.25 USD per token * 10**18
        now = int(time.time())
        with self.state.lock:
            quote = self.state.quotes.get(quote_id)
            if quote is None:
                quote = {
                    "id": quote_id,
                    "object": "quote",
                    "account_id": account,
                    "amount": amount,
                    "currency": "usd",
                    "chain_id": CHAIN_ID,
                    "asset": "pha",
                    "amount_atomic": atomic,
                    "exchange_rate": PRICE,
                    "address": address,
                    "payment_uri": f"ethereum:{self.args.token}@{CHAIN_ID}/transfer"
                    f"?address={address}&uint256={atomic}",
                    "status": "open",
                    "expires_at": now + 900,
                    "created": now,
                    "payment": None,
                    "deposit": None,
                }
                self.state.quotes[quote_id] = quote
            secret = f"{quote_id}_secret_{secrets.token_hex(24)}"
            self.state.secrets[quote_id] = secret
        return {**self.quote_view(quote), "client_secret": secret}

    def quote_view(self, quote: dict[str, Any]) -> dict[str, Any]:
        view = {k: v for k, v in quote.items() if k != "payment"}
        payment = quote["payment"]
        if payment is None:
            view["payment"] = None
        else:
            head = int(self.call("eth_blockNumber"), 16)
            final = quote["deposit"] is not None
            view["payment"] = {
                "status": "final" if final else "seen",
                "tx_hash": payment["tx_hash"],
                "amount_atomic": payment["amount_atomic"],
                "confirmations": None if final else head - payment["block"] + 1,
                "estimated_final_at": None if final else int(time.time()) + 60,
                "matches_quote": payment["amount_atomic"] == quote["amount_atomic"],
                "deposit": deposit_id(CHAIN_ID, payment["tx_hash"], payment["log_index"]),
            }
        return view

    def client_view(self, quote: dict[str, Any]) -> dict[str, Any]:
        deposit = self.state.deposits.get(quote["deposit"] or "")
        payment_status = "none"
        if deposit is not None:
            payment_status = {"confirmed": "confirming"}.get(deposit["status"], "credited")
        elif quote["payment"] is not None:
            payment_status = "seen"
        keys = (
            *("id", "object", "status", "amount", "currency", "asset", "chain_id"),
            *("amount_atomic", "address", "payment_uri", "expires_at"),
        )
        view = {k: quote[k] for k in keys}
        view.update(decimals=18, payment_status=payment_status, confirmations=None)
        return view

    def attestation(self, nonce: str) -> dict[str, Any]:
        public = self.key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        report = attestation_report_data(bytes.fromhex(nonce), public)
        return {
            "keyid": "settlement/v1",
            "settlement_pubkey": public.hex(),
            "report_data": report.hex(),
            "quote": "00" * 1024,
        }


def evidence() -> dict[str, Any]:
    events = [
        ("app-id", "e2e0000000000000000000000000000000000001"),
        ("compose-hash", "c0" * 32),
        ("os-image-hash", "05" * 32),
    ]
    return {
        "quote": "00" * 1024,
        "report_data": "00" * 64,
        "vm_config": "{}",
        "event_log": json.dumps(
            [{"imr": 3, "event": name, "event_payload": value} for name, value in events]
        ),
    }


def serve(fake: FakeTopup) -> ThreadingHTTPServer:
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            url = urlsplit(self.path)
            query = {k: v[0] for k, v in parse_qs(url.query).items()}
            parts = url.path.strip("/").split("/")
            if url.path == "/v1/attestation":
                self.send(HTTPStatus.OK, fake.attestation(query["nonce"]))
            elif url.path == "/evidences/quote.json":
                self.send(HTTPStatus.OK, evidence())
            elif parts[:2] == ["v1", "quotes"] and len(parts) == 3:
                quote = fake.state.quotes.get(parts[2])
                if quote is None:
                    self.error(HTTPStatus.NOT_FOUND)
                elif "client_secret" in query:
                    if fake.state.secrets.get(quote["id"]) != query["client_secret"]:
                        self.error(HTTPStatus.NOT_FOUND)
                    else:
                        self.send(HTTPStatus.OK, fake.client_view(quote), cors=True)
                else:
                    self.send(HTTPStatus.OK, fake.quote_view(quote))
            elif url.path == "/v1/deposits":
                data = [
                    {k: v for k, v in d.items() if not k.startswith("_")}
                    for d in fake.state.deposits.values()
                    if d["quote"] == query.get("quote", d["quote"])
                    and d["account_id"] == query.get("account_id", d["account_id"])
                ]
                body = {"object": "list", "url": "/v1/deposits", "has_more": False, "data": data}
                self.send(HTTPStatus.OK, body)
            else:
                self.error(HTTPStatus.NOT_FOUND)

        def do_POST(self) -> None:
            length = int(self.headers.get("content-length") or 0)
            body = json.loads(self.rfile.read(length))
            if self.path == "/v1/quotes":
                key = self.headers.get("idempotency-key", "").strip('"')
                self.send(HTTPStatus.OK, fake.create_quote(body, key))
            else:
                self.error(HTTPStatus.NOT_FOUND)

        def error(self, status: HTTPStatus) -> None:
            code = "resource_missing" if status == HTTPStatus.NOT_FOUND else "api_error"
            body = {"error": {"type": "invalid_request_error", "code": code, "message": code}}
            self.send(status, body, cors=True)

        def send(self, status: HTTPStatus, body: dict[str, Any], *, cors: bool = False) -> None:
            payload = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            if cors:
                self.send_header("access-control-allow-origin", "*")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, format: str, *args: Any) -> None:
            LOG.debug(format, *args)

    return ThreadingHTTPServer(("127.0.0.1", fake.args.port), Handler)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    for name in ("rpc", "token", "product-webhook", "settlement-seed", "factory"):
        parser.add_argument(f"--{name}", required=True)
    for name in ("implementation", "product", "treasury"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--port", type=int, required=True)
    logging.basicConfig(level=logging.INFO, format="fake_service %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    fake = FakeTopup(parser.parse_args())
    server = serve(fake)
    threading.Thread(target=fake.watch, daemon=True).start()
    try:
        server.serve_forever()
    finally:
        fake.stop.set()


if __name__ == "__main__":
    main()
