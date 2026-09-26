"""The settlement endpoint and webhook receiver: the product side of architecture section 11.

`SettlementService` enforces all six product obligations:

1. verify the RFC 9421 signature against the pinned `(keyid, public key)`;
2. keep idempotency records forever: the order row keyed by `provider_order_id` is never deleted;
3. commit the order, credit transaction, and `complete_order_payment` in one transaction before
   answering `accepted`, and find-or-create under a unique index so concurrency credits once;
4. enforce the product's own per-deposit and per-period caps;
5. verify the cited log with the product's own RPC: finalized, emitted by the approved token,
   `to` equal to an address the product computed for that workspace, and the exact amount;
6. recompute `deposit_id` from chain evidence and require `idempotency_key == "deposit:" + id`.

`topup-conformance` holds it to that contract (deploy/product/conformance.sh).
"""

from __future__ import annotations

import json
import logging
import time
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from http import HTTPStatus
from typing import Any
from urllib.parse import unquote, urlsplit

import httpx
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_sdk import SignatureError, verify_request, verify_webhook
from topup_sdk.addresses import deposit_id, forwarder_address, keccak256, lock_salt, same_address

from .config import CONFORMANCE_PROCESSING, SETTLEMENT_KEYID, ProductConfig
from .ledger import ORDER_FLOW_CODE, ORDER_PROVIDER, ProductLedger, StoredOrder

LOG = logging.getLogger(__name__)

TRANSFER_TOPIC = "0x" + keccak256(b"Transfer(address,address,uint256)").hex()


class TransientError(Exception):
    """A dependency is unavailable; answer 503 without recording a decision."""


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


@dataclass
class Answer:
    status: int
    body: dict[str, Any] | None = None


class SettlementService:
    """Product-side settlement contract (architecture section 11)."""

    def __init__(
        self,
        config: ProductConfig,
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
        amount = parse_decimal(payload.get("amount_minor"))
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
        amount = parse_decimal(evidence.get("amount_atomic"))
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
            # The conformance suite's processing account: held for review, never credited here.
            held = refusal is None and self.config.conformance and team_id == CONFORMANCE_PROCESSING
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
                    "rejected" if refusal else "processing" if held else "pending",
                    refusal,
                    now,
                ),
            )
            if refusal is None and not held:
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


def parse_decimal(value: object) -> int | None:
    if not isinstance(value, str) or not (value.isascii() and value.isdigit()):
        return None
    return int(value)


def _canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


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
