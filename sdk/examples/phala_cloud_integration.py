"""End-to-end Phala Cloud integration example for the crypto top-up service.

It plays the product side against a running service (the local sandbox, the Sepolia sandbox, or
staging). The product (`serve`) is a long-running service:

1. pins the service's settlement key from attestation;
2. serves the settlement endpoint and webhook receiver, with its ledger in SQLite;
3. serves its own account API, through which a user registers a workspace, gets a quote-first
   single-use address, and reads its deposits, credits, and webhook events. It holds the product
   key and calls the service on the user's behalf, as Phala Cloud's backend does.

The deposit driver (`deposit`) plays that user from an operator's machine: it registers a
workspace, gets a quote and recomputes its address locally, pays the exact locked amount with the
test token, polls until the deposit is credited, and checks that the product ledger credited the
locked amount exactly once and received the verified `deposit.credited` webhook.

With no mode, the example runs both in one process (`deploy/sandbox/run-local.sh`).

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
`uv run --project sdk/python python sdk/examples/phala_cloud_integration.py [MODE] --config FILE`
where MODE is `serve` or `deposit` (deploy/README.md, "Staging reference product").
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import re
import secrets
import signal
import sqlite3
import subprocess
import threading
import time
import uuid
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from dataclasses import dataclass, field, replace
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Protocol
from urllib.parse import quote, unquote, urlsplit

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import AttestationResponse, DepositResponse, RateLockResponse
from topup_sdk import (
    ApiError,
    AttestationError,
    RequestSigner,
    SignatureError,
    SigningAuth,
    TopupClient,
    load_public_key,
    verify_attestation_binding,
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
# The deposit driver signs its account API requests with this key id (see `AccountApi`).
DRIVER_KEYID = "driver/v1"
# Workspace ids and lock references in the account API: URL path segments without escaping.
ACCOUNT_REF = re.compile(r"[A-Za-z0-9._-]{1,64}")


@dataclass(frozen=True)
class SandboxConfig:
    """Everything the product needs; see deploy/sandbox/README.md for each field.

    The product key comes from `product_seed_file`, or, in a CVM, from the sealed environment
    variable named by `product_seed_env` (64 hexadecimal characters, as `topup-sdk keygen`
    writes them). The deposit driver needs neither: it calls the product's account API at
    `public_url`, signed with the driver key whose public key is `driver_public_key`.
    """

    service_url: str
    product_slug: str
    product_keyid: str
    route: str
    chain_id: int
    rpc_url: str
    factory: str
    implementation: str
    token: str
    token_symbol: str
    public_url: str
    listen_host: str = "127.0.0.1"
    listen_port: int = 8089
    product_seed_file: str | None = None
    product_seed_env: str | None = None
    ledger_path: str = ":memory:"
    driver_public_key: str | None = None
    payer: str | None = None
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
        if self.product_seed_file is not None:
            return RequestSigner.from_seed_file(self.product_keyid, self.product_seed_file)
        seed = os.environ.get(self.product_seed_env or "", "").strip()
        if not seed:
            raise MissingProductKeyError("no product_seed_file, and product_seed_env is unset")
        return RequestSigner.from_seed(self.product_keyid, bytes.fromhex(seed))

    def client(self) -> TopupClient:
        return TopupClient(self.service_url, self.product_slug, self.signer())


class TransientError(Exception):
    """A dependency is unavailable; answer 503 without recording a decision."""


class MissingProductKeyError(Exception):
    """The product key is not configured (in a CVM: not sealed yet)."""


# --- Chain access (the product's own RPC) -------------------------------------------------------


class JsonRpc:
    """Minimal JSON-RPC client over the product's own node."""

    def __init__(self, url: str, timeout: float = 10.0) -> None:
        self._http = httpx.Client(timeout=timeout)
        self._url = url

    def call(self, method: str, params: list[Any]) -> Any:
        # Messages name the method and the failure class only: the URL may carry an API key.
        try:
            response = self._http.post(
                self._url, json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
            )
            response.raise_for_status()
            body = response.json()
        except httpx.HTTPStatusError as error:
            raise TransientError(f"{method} answered HTTP {error.response.status_code}") from error
        except (httpx.HTTPError, ValueError) as error:
            raise TransientError(f"{method} failed: {type(error).__name__}") from error
        if "error" in body:
            code = body["error"].get("code") if isinstance(body["error"], dict) else None
            raise TransientError(f"{method} returned JSON-RPC error {code}")
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

    On Anvil the payer is `payer`, an unlocked development account. On Sepolia `cast` signs with
    a Foundry keystore holding a funded throwaway test key: the account named by `payer_account`
    (`cast wallet import`), or else the keystore file named by `ETH_KEYSTORE`. `cast` reads the
    keystore password from the mode-0600 file named by `ETH_PASSWORD`; no key is ever passed in
    the environment or on a command line. The test token's `mint` is public, so the payer mints
    what it pays and needs only Sepolia ETH for gas.
    """

    def __init__(self, config: SandboxConfig, rpc: JsonRpc) -> None:
        self._rpc = rpc
        self._rpc_url = config.rpc_url
        self._wallet: list[str] | None = None
        if config.payer_account is not None:
            self._wallet = ["--account", config.payer_account]
        elif os.environ.get("ETH_KEYSTORE"):
            self._wallet = []  # cast reads ETH_KEYSTORE and ETH_PASSWORD itself
        if self._wallet is None:
            if config.payer is None:
                raise ValueError("set payer (Anvil), payer_account, or ETH_KEYSTORE")
            self.address = config.payer
        else:
            self.address = self._cast("wallet", "address", *self._wallet).strip()

    def mint_and_transfer(self, token: str, to: str, amount_atomic: int) -> str:
        self.send(token, "mint(address,uint256)", self.address, amount_atomic)
        return self.send(token, "transfer(address,uint256)", to, amount_atomic)

    def send(self, contract: str, signature: str, address: str, amount: int) -> str:
        if self._wallet is None:
            data = _selector(signature) + _word(int(address, 16)) + _word(amount)
            tx_hash = str(
                self._rpc.call(
                    "eth_sendTransaction",
                    [{"from": self.address, "to": contract, "data": "0x" + data.hex()}],
                )
            )
        else:
            output = self._cast(
                "send",
                "--json",
                *self._wallet,
                "--rpc-url",
                self._rpc_url,
                contract,
                signature,
                address,
                str(amount),
            )
            tx_hash = str(json.loads(output)["transactionHash"])
        self._rpc.wait_for_receipt(tx_hash)
        return tx_hash

    @staticmethod
    def _cast(*arguments: str) -> str:
        return subprocess.run(
            ["cast", *arguments], check=True, capture_output=True, text=True
        ).stdout


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

    def all_events(self) -> list[dict[str, Any]]:
        """Every stored webhook event as `{"type", "data"}`, oldest first."""
        with self._lock:
            rows = self._connection.execute(
                "SELECT type, data FROM webhook_events ORDER BY received_at"
            ).fetchall()
        return [{"type": row[0], "data": json.loads(row[1])} for row in rows]

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
        except TransientError as error:
            # The reason is non-sensitive and lands in the service's retry evidence, which is
            # the only diagnostic when the product runs without logs.
            LOG.warning("settlement %s deferred: %s", key, error)
            return Answer(HTTPStatus.SERVICE_UNAVAILABLE, {"retry_reason": str(error)})
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
    """Serves `POST /settlements`, `GET /settlements/{key}`, `POST /webhooks`, `GET /healthz`,
    and, given an `AccountApi`, `/accounts`."""

    def __init__(
        self,
        settlement: SettlementService,
        webhooks: WebhookReceiver,
        accounts: AccountApi | None = None,
    ) -> None:
        self.settlement = settlement
        self.webhooks = webhooks
        self.accounts = accounts
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
                elif server.accounts is not None and server.accounts.handles(self.path):
                    self._send(server.accounts.handle("POST", self.path, headers, body))
                else:
                    self._send(Answer(HTTPStatus.NOT_FOUND))

            def do_GET(self) -> None:
                headers = dict(self.headers.items())
                if self.path.startswith(base_path + "/settlements/"):
                    self._send(server.settlement.handle_get(self.path, headers))
                elif self.path == base_path + "/healthz":
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


# --- Account API (the product's own API for its users) --------------------------------------------


class AccountApi:
    """The product's account API; the deposit driver uses it as a signed-in user would.

    - `POST /accounts` `{"account_id"}` registers a workspace (`register_team`);
    - `POST /accounts/{id}/quotes` `{"lock_ref", "amount_minor"}` creates a quote-first lock
      (`create_quote`) and returns the service's lock;
    - `GET /accounts/{id}` returns the workspace's deposits (from the service), its credits
      (from the ledger), and the verified webhook events for those deposits.

    The product calls the service with its own key on the user's behalf, as Phala Cloud's
    backend does. Requests must carry an RFC 9421 signature by the pinned driver key
    (`driver_public_key`, key id `driver/v1`), which stands in for user sessions and cannot sign
    service requests. Replays are bounded only by the five-minute freshness window; every
    operation is idempotent.
    """

    def __init__(self, config: SandboxConfig, ledger: ProductLedger, driver_key: Ed25519PublicKey):
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
                lock = create_quote(
                    self.config,
                    self._service(),
                    self.ledger,
                    team,
                    lock_ref=_account_ref(request.get("lock_ref")),
                    amount_minor=amount_minor,
                )
                return Answer(HTTPStatus.OK, lock.to_dict())
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
        deposits = list(self._service().list_deposits(team))
        ids = {str(deposit.id) for deposit in deposits}
        return {
            "account_id": team,
            "deposits": [deposit.to_dict() for deposit in deposits],
            "credits": [
                {"provider_order_id": key, "amount_minor": amount}
                for key, amount in self.ledger.credits_for(team)
            ],
            "events": [
                event
                for event in self.ledger.all_events()
                if event["data"].get("deposit_id") in ids
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


# --- Product flows --------------------------------------------------------------------------------


def pin_settlement_key(config: SandboxConfig, *, wait_s: float = 0) -> Ed25519PublicKey:
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


class TeamLedger(Protocol):
    def add_team(self, team_id: str, *, suspended: bool = False) -> None: ...

    def record_address(
        self, address: str, team_id: str, *, version: int | None = None, lock_ref: str | None = None
    ) -> None: ...


def register_team(
    config: SandboxConfig,
    client: TopupClient,
    ledger: TeamLedger,
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
    ledger: TeamLedger,
    team: str,
    *,
    lock_ref: str,
    amount_minor: int,
) -> RateLockResponse:
    """Creates a quote-first lock, recording its recomputed address before the request.

    Recording first means a crash between the two steps never leaves a paid quote address the
    product does not recognise; the service's answer must then match the recorded address.
    """
    expected = quote_address(config, team, lock_ref)
    ledger.record_address(expected, team, lock_ref=lock_ref)
    lock = client.create_rate_lock(team, lock_ref, amount_minor=amount_minor)
    if not same_address(expected, lock.address):
        raise RuntimeError("rate-lock address does not match the product's computation")
    return lock


def quote_address(config: SandboxConfig, team: str, lock_ref: str) -> str:
    return forwarder_address(
        config.factory, config.implementation, lock_salt(config.product_slug, team, lock_ref)
    )


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


@contextmanager
def product_service(config: SandboxConfig, *, pin_wait_s: float = 0) -> Iterator[ProductServer]:
    """Runs the product: settlement endpoint, webhook receiver, and account API."""
    if config.driver_public_key is None:
        raise ValueError("driver_public_key is required to serve the account API")
    settlement_key = pin_settlement_key(config, wait_s=pin_wait_s)
    ledger = ProductLedger(config.ledger_path)
    settlement = SettlementService(config, ledger, settlement_key, JsonRpc(config.rpc_url))
    accounts = AccountApi(config, ledger, load_public_key(config.driver_public_key))
    try:
        with ProductServer(settlement, WebhookReceiver(ledger, settlement_key), accounts) as server:
            LOG.info("product listening on %s:%s", config.listen_host, config.listen_port)
            yield server
    finally:
        accounts.close()


def serve(config: SandboxConfig) -> None:
    """Serves the product until SIGTERM or SIGINT."""
    stop = threading.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda *_: stop.set())
    with product_service(config, pin_wait_s=600):
        stop.wait()
    LOG.info("product stopped")


# --- Deposit driver -------------------------------------------------------------------------------


class ProductApiError(Exception):
    def __init__(self, status: int, body: str) -> None:
        super().__init__(f"product answered {status}: {body[:200]}")
        self.status = status


class ProductApi:
    """The deposit driver's client for the product's account API, signed with the driver key."""

    def __init__(self, public_url: str, signer: RequestSigner) -> None:
        self._base = public_url.rstrip("/")
        self._http = httpx.Client(auth=SigningAuth(signer), timeout=60)

    def __enter__(self) -> ProductApi:
        return self

    def __exit__(self, *_: object) -> None:
        self._http.close()

    def register(self, team: str) -> str:
        return str(self._call("POST", "/accounts", {"account_id": team})["address"])

    def quote(self, team: str, lock_ref: str, amount_minor: int) -> RateLockResponse:
        body = {"lock_ref": lock_ref, "amount_minor": amount_minor}
        return RateLockResponse.from_dict(
            self._call("POST", f"/accounts/{quote(team)}/quotes", body)
        )

    def account(self, team: str) -> dict[str, Any]:
        return self._call("GET", f"/accounts/{quote(team)}")

    def _call(self, method: str, path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        response = self._http.request(method, self._base + path, json=body)
        if response.status_code != HTTPStatus.OK:
            raise ProductApiError(response.status_code, response.text)
        value = response.json()
        if not isinstance(value, dict):
            raise ProductApiError(response.status_code, "not a JSON object")
        return value


def run_deposit(
    config: SandboxConfig,
    driver: RequestSigner,
    *,
    amount_minor: int,
    min_atomic: int = 0,
    until: str = "credited",
    timeout: float = 1800,
) -> None:
    """Registers a workspace through the product, pays one quote, and checks the credit."""
    rpc = JsonRpc(config.rpc_url)
    payer = Payer(config, rpc)
    with ProductApi(config.public_url, driver) as api:
        team = f"team-{uuid.uuid4().hex[:12]}"
        persistent = api.register(team)
        LOG.info("registered workspace %s (persistent address %s)", team, persistent)

        lock_ref = f"checkout-{uuid.uuid4().hex[:12]}"
        lock = api.quote(team, lock_ref, amount_minor)
        # Pay only an address recomputed here from the product slug, workspace, and lock_ref.
        if not same_address(quote_address(config, team, lock_ref), lock.address):
            raise RuntimeError("quote address does not match the driver's own computation")
        amount_atomic = int(lock.amount_atomic)
        LOG.info(
            "quote: pay %s atomic to %s before %s for %s minor (%s)",
            amount_atomic,
            lock.address,
            lock.expires_at.isoformat(),
            lock.credit_minor,
            lock.eip681_uri,
        )
        if amount_atomic < min_atomic:
            needed = -(-amount_minor * min_atomic // amount_atomic)
            raise RuntimeError(
                f"the quote locks {amount_atomic} atomic, below --min-atomic {min_atomic}; "
                f"nothing was paid; rerun with --amount-minor of at least {needed}"
            )

        tx_hash = payer.mint_and_transfer(config.token, lock.address, amount_atomic)
        LOG.info("paid in %s from %s; waiting for finality and credit", tx_hash, payer.address)
        states = {"credited", "swept"} if until == "credited" else {"swept"}
        deposit, view = _wait_for_credit(api, team, lock.address, states, timeout)
        event = next(
            event["data"]
            for event in view["events"]
            if event["type"] == "deposit.credited"
            and event["data"]["deposit_id"] == str(deposit.id)
        )
        if event["amount_minor"] != lock.credit_minor:
            raise RuntimeError("credited amount differs from the locked quote")
        credits = [(c["provider_order_id"], c["amount_minor"]) for c in view["credits"]]
        if credits != [(f"deposit:{deposit.id}", int(lock.credit_minor))]:
            raise RuntimeError(f"unexpected product ledger credits: {credits}")
        LOG.info(
            "deposit %s is %s: credited %s minor (transaction %s); the ledger holds one credit",
            deposit.id,
            deposit.state,
            lock.credit_minor,
            event["destination_tx_id"],
        )


def _wait_for_credit(
    api: ProductApi, team: str, address: str, states: set[str], timeout: float
) -> tuple[DepositResponse, dict[str, Any]]:
    """Polls the product until a deposit to `address` is in `states` and its credit and
    `deposit.credited` webhook are in the product ledger."""
    deadline = time.monotonic() + timeout
    last_state = None
    while time.monotonic() < deadline:
        try:
            view = api.account(team)
        except (httpx.HTTPError, ProductApiError) as error:
            if isinstance(error, ProductApiError) and error.status < 500:
                raise
            LOG.warning("product unavailable: %s", error)
            time.sleep(5)
            continue
        for item in view["deposits"]:
            deposit = DepositResponse.from_dict(item)
            if not same_address(deposit.address, address):
                continue
            if deposit.state != last_state:
                LOG.info("deposit %s is %s", deposit.id, deposit.state)
                last_state = deposit.state
            credited = any(
                event["type"] == "deposit.credited"
                and event["data"]["deposit_id"] == str(deposit.id)
                for event in view["events"]
            )
            if deposit.state in states and credited and view["credits"]:
                return deposit, view
        time.sleep(5)
    raise TimeoutError(f"no deposit to {address} reached {sorted(states)} in {timeout:.0f}s")


def run_example(config: SandboxConfig) -> None:
    """Serves the product and drives one deposit through it, in one process."""
    driver = RequestSigner.from_seed(DRIVER_KEYID, secrets.token_bytes(32))
    config = replace(config, driver_public_key=driver.public_key_base64())
    with product_service(config):
        run_deposit(config, driver, amount_minor=2500, timeout=420)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "mode",
        nargs="?",
        choices=["serve", "deposit"],
        help="serve the product, or drive one deposit through a running product; "
        "without a mode, both in one process",
    )
    parser.add_argument("--config", default=os.environ.get("SANDBOX_CONFIG"))
    deposit = parser.add_argument_group("deposit")
    deposit.add_argument("--driver-seed-file", help="seed file of the driver key (driver/v1)")
    deposit.add_argument("--amount-minor", type=int, default=2500, help="quote amount in cents")
    deposit.add_argument(
        "--min-atomic",
        type=int,
        default=0,
        help="refuse to pay a quote locking fewer atomic units (the route's min_flush_atomic)",
    )
    deposit.add_argument("--until", choices=["credited", "swept"], default="credited")
    deposit.add_argument("--timeout", type=float, default=1800, help="seconds to wait")
    args = parser.parse_args()
    if not args.config:
        parser.error("--config or SANDBOX_CONFIG is required")
    if args.mode == "deposit" and not args.driver_seed_file:
        parser.error("deposit needs --driver-seed-file")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    config = SandboxConfig.load(args.config)
    if args.mode == "serve":
        serve(config)
        return 0
    if args.mode == "deposit":
        run_deposit(
            config,
            RequestSigner.from_seed_file(DRIVER_KEYID, args.driver_seed_file),
            amount_minor=args.amount_minor,
            min_atomic=args.min_atomic,
            until=args.until,
            timeout=args.timeout,
        )
    else:
        run_example(config)
    print("phala_cloud_integration: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
