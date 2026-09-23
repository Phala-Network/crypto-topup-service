"""End-to-end Phala Cloud integration example for the crypto top-up service.

It plays the product side against a running service (the local sandbox or the Sepolia sandbox):

1. pins the service's settlement key from attestation;
2. starts the product's settlement endpoint and webhook receiver;
3. registers a workspace, gets a quote-first single-use address, and recomputes it locally;
4. pays the exact locked amount with the sandbox test token;
5. polls the deposit until it is credited and waits for the verified `deposit.credited` webhook;
6. checks that the product ledger credited the locked amount exactly once.

The settlement endpoint is the reference for the Phala Cloud implementation (docs/architecture.md
section 11 and the monorepo E2 issue). It enforces all six product obligations:

1. verify the RFC 9421 signature against the pinned `(keyid, public key)`;
2. keep idempotency records forever: the order row keyed by `provider_order_id` is never deleted;
3. commit the order, credit transaction, and `complete_order_payment` in one transaction before
   answering `accepted`, and find-or-create under a unique index so concurrency credits once;
4. enforce the product's own per-deposit and per-period caps;
5. verify the cited log with the product's own RPC: finalized, emitted by the approved token,
   `to` equal to an address the product computed for that workspace, and the exact amount;
6. recompute `deposit_id` from chain evidence and require `idempotency_key == "deposit:" + id`.

Run it with `deploy/sandbox/run-local.sh`, or directly:
`uv run --project sdk/python python sdk/examples/phala_cloud_integration.py --config FILE`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import logging
import os
import secrets
import sqlite3
import subprocess
import threading
import time
import uuid
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlsplit

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import RateLockResponse
from topup_sdk import (
    RequestSigner,
    SignatureError,
    TopupClient,
    load_public_key,
    verify_request,
    verify_webhook,
)
from topup_sdk.addresses import (
    deposit_id,
    forwarder_address,
    keccak256,
    lock_salt,
    persistent_salt,
    same_address,
)

LOG = logging.getLogger("phala_cloud_integration")

SETTLEMENT_KEYID = "settlement/v1"
TRANSFER_TOPIC = "0x" + keccak256(b"Transfer(address,address,uint256)").hex()
MAX_BODY_BYTES = 1024 * 1024
ORDER_FLOW_CODE = "crypto-top-up"
ORDER_PROVIDER = "crypto_topup"


@dataclass(frozen=True)
class SandboxConfig:
    """Everything the product needs; see deploy/sandbox/README.md for each field."""

    service_url: str
    product_slug: str
    product_keyid: str
    product_seed_file: str
    route: str
    chain_id: int
    rpc_url: str
    factory: str
    implementation: str
    token: str
    token_symbol: str
    listen_host: str
    listen_port: int
    public_url: str
    payer: str
    payer_account: str | None = None
    unsupported_token: str | None = None
    settlement_public_key: str | None = None
    per_deposit_cap_minor: int = 100_000
    per_period_cap_minor: int = 500_000
    period_seconds: int = 24 * 60 * 60
    restart_command: list[str] = field(default_factory=list)

    @classmethod
    def load(cls, path: str | Path) -> SandboxConfig:
        values = json.loads(Path(path).read_text(encoding="utf-8"))
        return cls(**values)

    def signer(self) -> RequestSigner:
        return RequestSigner.from_seed_file(self.product_keyid, self.product_seed_file)

    def client(self) -> TopupClient:
        return TopupClient(self.service_url, self.product_slug, self.signer())


class TransientError(Exception):
    """A dependency is unavailable; answer 503 without recording a decision."""


# --- Chain access (the product's own RPC) -------------------------------------------------------


class JsonRpc:
    """Minimal JSON-RPC client over the product's own node."""

    def __init__(self, url: str, timeout: float = 10.0) -> None:
        self._http = httpx.Client(timeout=timeout)
        self._url = url

    def call(self, method: str, params: list[Any]) -> Any:
        try:
            response = self._http.post(
                self._url, json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
            )
            response.raise_for_status()
            body = response.json()
        except (httpx.HTTPError, ValueError) as error:
            raise TransientError(f"{method} failed") from error
        if "error" in body:
            raise TransientError(f"{method} returned an error")
        return body["result"]

    def finalized_block_number(self) -> int:
        return int(self.call("eth_getBlockByNumber", ["finalized", False])["number"], 16)

    def wait_for_receipt(self, tx_hash: str, timeout: float = 120.0) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            receipt = self.call("eth_getTransactionReceipt", [tx_hash])
            if receipt is not None:
                if receipt["status"] != "0x1":
                    raise RuntimeError(f"transaction {tx_hash} reverted")
                return dict(receipt)
            time.sleep(1)
        raise TimeoutError(f"transaction {tx_hash} was not mined")


class Payer:
    """Sends sandbox test-token transactions.

    On Anvil the payer is an unlocked development account. On Sepolia set `payer_account` to a
    Foundry keystore account holding a funded throwaway test key (`cast wallet import`); `cast`
    signs with it and reads the keystore password from the mode-0600 file named by `ETH_PASSWORD`.
    """

    def __init__(self, config: SandboxConfig, rpc: JsonRpc) -> None:
        self._config = config
        self._rpc = rpc

    def mint_and_transfer(self, token: str, to: str, amount_atomic: int) -> str:
        self.send(token, "mint(address,uint256)", self._config.payer, amount_atomic)
        return self.send(token, "transfer(address,uint256)", to, amount_atomic)

    def send(self, contract: str, signature: str, address: str, amount: int) -> str:
        account = self._config.payer_account
        if account is None:
            data = _selector(signature) + _word(int(address, 16)) + _word(amount)
            tx_hash = str(
                self._rpc.call(
                    "eth_sendTransaction",
                    [{"from": self._config.payer, "to": contract, "data": "0x" + data.hex()}],
                )
            )
        else:
            output = subprocess.run(
                [
                    "cast",
                    "send",
                    "--json",
                    "--account",
                    account,
                    "--rpc-url",
                    self._config.rpc_url,
                    contract,
                    signature,
                    address,
                    str(amount),
                ],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            tx_hash = str(json.loads(output)["transactionHash"])
        self._rpc.wait_for_receipt(tx_hash)
        return tx_hash


def _selector(signature: str) -> bytes:
    return keccak256(signature.encode("ascii"))[:4]


def _word(value: int) -> bytes:
    return value.to_bytes(32, "big")


# --- Product ledger ------------------------------------------------------------------------------

SCHEMA = """
CREATE TABLE IF NOT EXISTS teams (
    id TEXT PRIMARY KEY,
    suspended INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS team_addresses (
    address TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    kind TEXT NOT NULL,
    version INTEGER,
    lock_ref TEXT
);
-- team_id is NULL only for durable refusals of keys that name no known workspace.
CREATE TABLE IF NOT EXISTS orders (
    id TEXT PRIMARY KEY,
    team_id TEXT REFERENCES teams (id),
    provider TEXT NOT NULL,
    order_flow_code TEXT NOT NULL,
    provider_order_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    status TEXT NOT NULL,
    reason TEXT,
    credit_transaction_id TEXT,
    created_at REAL NOT NULL
);
-- Phala Cloud's partial unique index; the key is also looked up across teams before insert.
CREATE UNIQUE INDEX IF NOT EXISTS orders_crypto_topup_provider_order
    ON orders (team_id, provider_order_id) WHERE order_flow_code = 'crypto-top-up';
CREATE TABLE IF NOT EXISTS credit_transactions (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    order_id TEXT NOT NULL UNIQUE REFERENCES orders (id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    funding_source TEXT NOT NULL,
    created_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS webhook_events (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    data TEXT NOT NULL,
    received_at REAL NOT NULL
);
"""


@dataclass(frozen=True)
class StoredOrder:
    provider_order_id: str
    team_id: str | None
    payload: dict[str, Any]
    status: str
    reason: str | None
    credit_transaction_id: str | None

    def answer(self, *, include_payload: bool) -> dict[str, Any]:
        """The settlement-contract answer; `GET` always returns the original payload."""
        body: dict[str, Any] = {"status": self.status}
        if self.status == "accepted":
            body["destination_tx_id"] = self.credit_transaction_id
        if self.status == "rejected":
            body["reason"] = self.reason
        if include_payload:
            body["payload"] = self.payload
        return body


class ProductLedger:
    """SQLite stand-in for the product database; every write is one serialized transaction."""

    def __init__(self, path: str = ":memory:") -> None:
        self._connection = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self._connection.execute("PRAGMA foreign_keys = ON")
        self._connection.executescript(SCHEMA)
        self._lock = threading.RLock()
        self.events_changed = threading.Condition(self._lock)

    @contextmanager
    def transaction(self) -> Iterator[sqlite3.Connection]:
        with self._lock:
            self._connection.execute("BEGIN IMMEDIATE")
            try:
                yield self._connection
            except BaseException:
                self._connection.execute("ROLLBACK")
                raise
            self._connection.execute("COMMIT")

    def add_team(self, team_id: str, *, suspended: bool = False) -> None:
        with self.transaction() as db:
            db.execute(
                "INSERT INTO teams (id, suspended) VALUES (?, ?) "
                "ON CONFLICT (id) DO UPDATE SET suspended = excluded.suspended",
                (team_id, int(suspended)),
            )

    def team_suspended(self, team_id: str) -> bool | None:
        with self._lock:
            row = self._connection.execute(
                "SELECT suspended FROM teams WHERE id = ?", (team_id,)
            ).fetchone()
        return None if row is None else bool(row[0])

    def record_address(
        self, address: str, team_id: str, *, version: int | None = None, lock_ref: str | None = None
    ) -> None:
        kind = "persistent" if lock_ref is None else "lock"
        with self.transaction() as db:
            db.execute(
                "INSERT OR IGNORE INTO team_addresses (address, team_id, kind, version, lock_ref) "
                "VALUES (?, ?, ?, ?, ?)",
                (address.lower(), team_id, kind, version, lock_ref),
            )

    def address_owner(self, address: str) -> str | None:
        with self._lock:
            row = self._connection.execute(
                "SELECT team_id FROM team_addresses WHERE address = ?", (address.lower(),)
            ).fetchone()
        return None if row is None else str(row[0])

    def find_order(self, provider_order_id: str) -> StoredOrder | None:
        with self._lock:
            return self._find_order(self._connection, provider_order_id)

    def credited_since(self, db: sqlite3.Connection, team_id: str, since: float) -> int:
        row = db.execute(
            "SELECT COALESCE(SUM(amount_minor), 0) FROM credit_transactions "
            "WHERE team_id = ? AND created_at >= ?",
            (team_id, since),
        ).fetchone()
        return int(row[0])

    def credits_for(self, team_id: str) -> list[tuple[str, int]]:
        with self._lock:
            rows = self._connection.execute(
                "SELECT o.provider_order_id, c.amount_minor FROM credit_transactions c "
                "JOIN orders o ON o.id = c.order_id WHERE c.team_id = ? ORDER BY c.created_at",
                (team_id,),
            ).fetchall()
        return [(str(key), int(amount)) for key, amount in rows]

    def record_event(self, event_id: str, event_type: str, data: Mapping[str, Any]) -> bool:
        """Stores a webhook once; returns False for a duplicate delivery."""
        with self.transaction() as db:
            inserted = db.execute(
                "INSERT OR IGNORE INTO webhook_events (id, type, data, received_at) "
                "VALUES (?, ?, ?, ?)",
                (event_id, event_type, json.dumps(data, sort_keys=True), time.time()),
            ).rowcount
            self.events_changed.notify_all()
        return inserted == 1

    def events(self, event_type: str) -> list[dict[str, Any]]:
        with self._lock:
            rows = self._connection.execute(
                "SELECT data FROM webhook_events WHERE type = ? ORDER BY received_at", (event_type,)
            ).fetchall()
        return [json.loads(row[0]) for row in rows]

    def wait_for_event(
        self, event_type: str, matches: Callable[[dict[str, Any]], bool], timeout: float
    ) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        with self.events_changed:
            while True:
                for event in self.events(event_type):
                    if matches(event):
                        return event
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"no matching {event_type} webhook within {timeout:.0f}s")
                self.events_changed.wait(remaining)

    @staticmethod
    def _find_order(db: sqlite3.Connection, provider_order_id: str) -> StoredOrder | None:
        row = db.execute(
            "SELECT provider_order_id, team_id, payload, status, reason, credit_transaction_id "
            "FROM orders WHERE order_flow_code = ? AND provider_order_id = ?",
            (ORDER_FLOW_CODE, provider_order_id),
        ).fetchone()
        if row is None:
            return None
        return StoredOrder(row[0], row[1], json.loads(row[2]), row[3], row[4], row[5])


# --- Settlement endpoint --------------------------------------------------------------------------


@dataclass
class Answer:
    status: int
    body: dict[str, Any] | None = None


class SettlementService:
    """Product-side settlement contract (architecture section 11)."""

    def __init__(
        self,
        config: SandboxConfig,
        ledger: ProductLedger,
        settlement_key: Ed25519PublicKey,
        rpc: JsonRpc,
    ) -> None:
        self.config = config
        self.ledger = ledger
        self.settlement_key = settlement_key
        self.rpc = rpc
        self.settlement_path = urlsplit(config.public_url).path.rstrip("/") + "/settlements"

    def handle_post(self, target: str, headers: Mapping[str, str], body: bytes) -> Answer:
        # Obligation 1: pinned signature over method, target URI, digest, and idempotency key.
        key = self._verified_key("POST", target, headers, body)
        if key is None:
            return Answer(HTTPStatus.UNAUTHORIZED)
        try:
            payload = json.loads(body)
        except ValueError:
            return Answer(HTTPStatus.BAD_REQUEST)
        if not isinstance(payload, dict) or payload.get("idempotency_key") != key:
            return Answer(HTTPStatus.UNPROCESSABLE_ENTITY)

        # Obligation 2: an existing record answers every replay, forever.
        existing = self.ledger.find_order(key)
        if existing is not None:
            return self._replay(existing, payload)

        try:
            refusal = self.refusal_reason(key, payload)
        except TransientError:
            LOG.warning("settlement %s deferred: dependency unavailable", key)
            return Answer(HTTPStatus.SERVICE_UNAVAILABLE)
        return self._commit(key, payload, refusal)

    def handle_get(self, target: str, headers: Mapping[str, str]) -> Answer:
        key = self._verified_key("GET", target, headers, b"")
        path_key = unquote(urlsplit(target).path.removeprefix(self.settlement_path + "/"))
        if key is None or key != path_key:
            return Answer(HTTPStatus.UNAUTHORIZED)
        order = self.ledger.find_order(key)
        if order is None:
            return Answer(HTTPStatus.NOT_FOUND)
        return Answer(HTTPStatus.OK, order.answer(include_payload=True))

    def refusal_reason(self, key: str, payload: dict[str, Any]) -> str | None:
        """Returns a durable business refusal, `None` to accept, or raises `TransientError`."""
        if payload.get("version") != 1 or payload.get("source") != "crypto_deposit":
            return "unsupported_payload"
        if payload.get("unit") != "USD":
            return "unsupported_unit"
        team_id = payload.get("account_id")
        suspended = self.ledger.team_suspended(team_id) if isinstance(team_id, str) else None
        if not isinstance(team_id, str) or suspended is None:
            return "unknown_account"
        if suspended:
            return "account_suspended"
        amount = _decimal(payload.get("amount_minor"))
        if amount is None or amount <= 0:
            return "invalid_amount"
        # Obligation 4: the product's own caps, independent of the service's.
        if amount > self.config.per_deposit_cap_minor:
            return "per_deposit_cap"
        evidence = payload.get("evidence")
        if not isinstance(evidence, dict):
            return "invalid_chain_evidence"
        # Obligation 6: one chain event can only ever be credited under its own key.
        try:
            expected_key = "deposit:" + str(
                deposit_id(int(evidence["chain_id"]), evidence["tx_hash"], evidence["log_index"])
            )
        except (KeyError, TypeError, ValueError):
            return "invalid_chain_evidence"
        if key != expected_key:
            return "deposit_identity_mismatch"
        # Obligation 5: the cited log, checked against the product's own RPC.
        return self.chain_evidence_problem(team_id, evidence)

    def chain_evidence_problem(self, team_id: str, evidence: dict[str, Any]) -> str | None:
        if evidence.get("chain_id") != self.config.chain_id:
            return "unapproved_chain"
        if evidence.get("route") != self.config.route:
            return "unapproved_route"
        asset = evidence.get("asset_contract")
        if not isinstance(asset, str) or not same_address(asset, self.config.token):
            return "unapproved_asset"
        to = evidence.get("to")
        if not isinstance(to, str) or not self._team_address(team_id, to, evidence):
            return "address_not_owned_by_account"
        amount = _decimal(evidence.get("amount_atomic"))
        if amount is None:
            return "invalid_chain_evidence"

        try:
            return self._finalized_log_problem(evidence, to, amount)
        except (KeyError, IndexError, TypeError, ValueError) as error:
            # A malformed receipt, block, or log says nothing durable about the deposit.
            raise TransientError("malformed chain data from the product node") from error

    def _finalized_log_problem(self, evidence: dict[str, Any], to: str, amount: int) -> str | None:
        """Checks the cited log against the product's own node.

        Only facts that cannot change once the block is final are stored as refusals: a reverted
        transaction, or a finalized log that is missing, emitted by another contract, or names
        another recipient or amount. The service settles only after two providers saw finality,
        so a missing receipt, an unfinalized or non-canonical block on our node, or malformed
        data is node lag, a pruned index, or a mixed backend: answer 503 and store nothing
        (architecture section 11, obligation 5).
        """
        receipt = self.rpc.call("eth_getTransactionReceipt", [evidence["tx_hash"]])
        if receipt is None:
            raise TransientError("receipt not available on the product node")
        block_number = int(receipt["blockNumber"], 16)
        if self.rpc.finalized_block_number() < block_number:
            raise TransientError("block not finalized on the product node yet")
        canonical = self.rpc.call("eth_getBlockByNumber", [receipt["blockNumber"], False])
        if canonical is None or canonical["hash"] != receipt["blockHash"]:
            raise TransientError("receipt block is not canonical on the product node")
        if receipt["status"] != "0x1":
            return "transaction_reverted"
        log = next(
            (
                entry
                for entry in receipt["logs"]
                if int(entry["logIndex"], 16) == evidence.get("log_index")
            ),
            None,
        )
        if log is None:
            return "log_not_found"
        if not same_address(log["address"], self.config.token):
            return "log_not_emitted_by_asset"
        topics = log["topics"]
        if len(topics) != 3 or topics[0] != TRANSFER_TOPIC:
            return "log_not_a_transfer"
        if not same_address("0x" + topics[2][-40:], to):
            return "log_recipient_mismatch"
        if int(log["data"], 16) != amount:
            return "log_amount_mismatch"
        return None

    def _team_address(self, team_id: str, to: str, evidence: dict[str, Any]) -> bool:
        """`to` must be an address this product computed itself for the account.

        A cited `lock_ref` is recomputed directly from the salt inputs. Payments that do not
        consume a lock (late, wrong amount) carry `lock_ref: null`, so the product also records
        every quote address it computes, before asking the service for the quote.
        """
        lock_ref = evidence.get("lock_ref")
        if lock_ref is not None:
            if not isinstance(lock_ref, str):
                return False
            salt = lock_salt(self.config.product_slug, team_id, lock_ref)
            expected = forwarder_address(self.config.factory, self.config.implementation, salt)
            return same_address(expected, to)
        return self.ledger.address_owner(to) == team_id

    def _commit(self, key: str, payload: dict[str, Any], refusal: str | None) -> Answer:
        team_id = None if refusal == "unknown_account" else str(payload["account_id"])
        now = time.time()
        # Obligation 3: find-or-create, credit, and complete in one transaction, then answer.
        # SQLite's BEGIN IMMEDIATE serializes every writer. On PostgreSQL, lock the team row
        # (SELECT ... FOR UPDATE) before the per-period cap sum so concurrent deposits cannot
        # both pass it, and also make provider_order_id unique across teams for this flow:
        # the per-team partial index does not cover refusals stored with team_id NULL.
        with self.ledger.transaction() as db:
            existing = ProductLedger._find_order(db, key)
            if existing is not None:
                return self._replay(existing, payload)
            if refusal is None and team_id is not None:
                already = self.ledger.credited_since(db, team_id, now - self.config.period_seconds)
                if already + int(payload["amount_minor"]) > self.config.per_period_cap_minor:
                    refusal = "per_period_cap"
            order_id = str(uuid.uuid4())
            db.execute(
                "INSERT INTO orders (id, team_id, provider, order_flow_code, provider_order_id, "
                "payload, status, reason, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    order_id,
                    team_id,
                    ORDER_PROVIDER,
                    ORDER_FLOW_CODE,
                    key,
                    json.dumps(payload),
                    "rejected" if refusal else "pending",
                    refusal,
                    now,
                ),
            )
            if refusal is None:
                credit_id = f"ctx_{uuid.uuid4().hex}"
                evidence = payload["evidence"]
                db.execute(
                    "INSERT INTO credit_transactions "
                    "(id, team_id, order_id, amount_minor, funding_source, created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?)",
                    (
                        credit_id,
                        team_id,
                        order_id,
                        int(payload["amount_minor"]),
                        f"crypto:{self.config.token_symbol}:{evidence['chain_id']}",
                        now,
                    ),
                )
                # complete_order_payment, in the same transaction as the credit.
                db.execute(
                    "UPDATE orders SET status = 'accepted', credit_transaction_id = ? WHERE id = ?",
                    (credit_id, order_id),
                )
            order = ProductLedger._find_order(db, key)
        if order is None:
            raise RuntimeError("order vanished after commit")
        LOG.info("settlement %s -> %s %s", key, order.status, order.reason or "")
        return Answer(HTTPStatus.OK, order.answer(include_payload=False))

    def _replay(self, order: StoredOrder, payload: dict[str, Any]) -> Answer:
        if _canonical(order.payload) != _canonical(payload):
            return Answer(HTTPStatus.UNPROCESSABLE_ENTITY)
        return Answer(HTTPStatus.OK, order.answer(include_payload=False))

    def _verified_key(
        self, method: str, target: str, headers: Mapping[str, str], body: bytes
    ) -> str | None:
        # The target URI comes from our configured public URL, never from the Host header.
        public = urlsplit(self.config.public_url)
        try:
            verified = verify_request(
                method=method,
                target_uri=f"{public.scheme}://{public.netloc}{target}",
                headers=headers,
                body=body,
                public_key=self.settlement_key,
                keyid=SETTLEMENT_KEYID,
                require_idempotency_key=True,
            )
        except SignatureError:
            return None
        return verified.idempotency_key


def _decimal(value: object) -> int | None:
    if not isinstance(value, str) or not (value.isascii() and value.isdigit()):
        return None
    return int(value)


def _canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


# --- Webhook receiver -----------------------------------------------------------------------------


class WebhookReceiver:
    """Verifies Standard Webhooks deliveries and stores each event once."""

    def __init__(self, ledger: ProductLedger, settlement_key: Ed25519PublicKey) -> None:
        self.ledger = ledger
        self.settlement_key = settlement_key

    def handle(self, headers: Mapping[str, str], body: bytes) -> Answer:
        try:
            event = verify_webhook(headers, body, self.settlement_key)
        except SignatureError:
            return Answer(HTTPStatus.UNAUTHORIZED)
        if self.ledger.record_event(event.id, event.type, event.data):
            LOG.info("webhook %s %s", event.type, event.data.get("deposit_id", ""))
        # Events never move balances: notify the user, refresh history, nothing more.
        return Answer(HTTPStatus.NO_CONTENT)


# --- HTTP server ----------------------------------------------------------------------------------


class ProductServer:
    """Serves `POST /settlements`, `GET /settlements/{key}`, and `POST /webhooks`."""

    def __init__(self, settlement: SettlementService, webhooks: WebhookReceiver) -> None:
        self.settlement = settlement
        self.webhooks = webhooks
        config = settlement.config
        base_path = urlsplit(config.public_url).path.rstrip("/")
        server = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                body = self._body()
                if body is None:
                    return
                headers = dict(self.headers.items())
                if self.path == base_path + "/settlements":
                    self._send(server.settlement.handle_post(self.path, headers, body))
                elif self.path == base_path + "/webhooks":
                    self._send(server.webhooks.handle(headers, body))
                else:
                    self._send(Answer(HTTPStatus.NOT_FOUND))

            def do_GET(self) -> None:
                if self.path.startswith(base_path + "/settlements/"):
                    self._send(server.settlement.handle_get(self.path, dict(self.headers.items())))
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


# --- Integration flow -----------------------------------------------------------------------------


def pin_settlement_key(config: SandboxConfig, client: TopupClient) -> Ed25519PublicKey:
    """Returns the settlement key, checking the attestation binds it to a fresh nonce.

    Production integrators must also verify the TDX quote with the dstack verification flow
    (deploy/README.md) and then pin `(keyid, public key)` in configuration; a configured
    `settlement_public_key` skips the fetch.
    """
    if config.settlement_public_key is not None:
        return load_public_key(config.settlement_public_key)
    nonce = secrets.token_bytes(32)
    evidence = client.attestation(nonce)
    public_key = bytes.fromhex(evidence.settlement_pubkey)
    if evidence.keyid != SETTLEMENT_KEYID:
        raise RuntimeError("attestation names an unexpected settlement key id")
    if bytes.fromhex(evidence.report_data) != hashlib.sha256(nonce + public_key).digest():
        raise RuntimeError("attestation report data does not bind the settlement key")
    LOG.warning("pinned settlement key from attestation; verify the quote before production")
    return load_public_key(evidence.settlement_pubkey)


def register_team(
    config: SandboxConfig,
    client: TopupClient,
    ledger: ProductLedger,
    team: str,
    *,
    suspended: bool = False,
) -> str:
    """Registers a workspace and records its persistent address after recomputing it."""
    ledger.add_team(team, suspended=suspended)
    client.register_account(team)
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
    config: SandboxConfig,
    client: TopupClient,
    ledger: ProductLedger,
    team: str,
    *,
    lock_ref: str,
    amount_minor: int,
) -> RateLockResponse:
    """Creates a quote-first lock, recording its recomputed address before the request.

    Recording first means a crash between the two steps never leaves a paid quote address the
    product does not recognise; the service's answer must then match the recorded address.
    """
    salt = lock_salt(config.product_slug, team, lock_ref)
    expected = forwarder_address(config.factory, config.implementation, salt)
    ledger.record_address(expected, team, lock_ref=lock_ref)
    lock = client.create_rate_lock(team, lock_ref, amount_minor=amount_minor)
    if not same_address(expected, lock.address):
        raise RuntimeError("rate-lock address does not match the product's computation")
    return lock


def wait_for_deposit(
    client: TopupClient, team: str, address: str, states: set[str], timeout: float
) -> Any:
    """Polls the account's deposits until one to `address` reaches one of `states`."""
    deadline = time.monotonic() + timeout
    last_state = None
    while time.monotonic() < deadline:
        for deposit in client.list_deposits(team):
            if same_address(deposit.address, address):
                if deposit.state != last_state:
                    LOG.info("deposit %s is %s", deposit.id, deposit.state)
                    last_state = deposit.state
                if deposit.state in states:
                    return deposit
        time.sleep(2)
    raise TimeoutError(f"no deposit to {address} reached {sorted(states)} in {timeout:.0f}s")


def run_example(config: SandboxConfig) -> None:
    rpc = JsonRpc(config.rpc_url)
    payer = Payer(config, rpc)
    ledger = ProductLedger()
    with config.client() as client:
        settlement_key = pin_settlement_key(config, client)
        settlement = SettlementService(config, ledger, settlement_key, rpc)
        with ProductServer(settlement, WebhookReceiver(ledger, settlement_key)):
            team = f"team-{uuid.uuid4().hex[:12]}"
            register_team(config, client, ledger, team)
            LOG.info("registered workspace %s", team)

            lock_ref = f"checkout-{uuid.uuid4().hex[:12]}"
            lock = create_quote(config, client, ledger, team, lock_ref=lock_ref, amount_minor=2500)
            LOG.info(
                "quote: pay %s atomic to %s before %s for %s minor (%s)",
                lock.amount_atomic,
                lock.address,
                lock.expires_at.isoformat(),
                lock.credit_minor,
                lock.eip681_uri,
            )
            if client.get_rate_lock(team, lock_ref).address != lock.address:
                raise RuntimeError("resumed checkout does not match the created lock")

            tx_hash = payer.mint_and_transfer(config.token, lock.address, int(lock.amount_atomic))
            LOG.info("paid in %s; waiting for finality and credit", tx_hash)
            deposit = wait_for_deposit(client, team, lock.address, {"credited", "swept"}, 300)
            event = ledger.wait_for_event(
                "deposit.credited", lambda data: data["deposit_id"] == str(deposit.id), 120
            )
            if event["amount_minor"] != lock.credit_minor:
                raise RuntimeError("credited amount differs from the locked quote")
            credits = ledger.credits_for(team)
            if credits != [(f"deposit:{deposit.id}", int(lock.credit_minor))]:
                raise RuntimeError(f"unexpected product ledger credits: {credits}")
            LOG.info(
                "credited %s minor for deposit %s (transaction %s); ledger holds one credit",
                lock.credit_minor,
                deposit.id,
                event["destination_tx_id"],
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--config", default=os.environ.get("SANDBOX_CONFIG"), required=False)
    args = parser.parse_args()
    if not args.config:
        parser.error("--config or SANDBOX_CONFIG is required")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    run_example(SandboxConfig.load(args.config))
    print("phala_cloud_integration: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
