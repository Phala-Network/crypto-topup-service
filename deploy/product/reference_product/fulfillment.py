"""The webhook receiver and fulfillment: the product side of `deposit.credited`.

The service credits a deposit once it is final, priced, and screened, and tells the product with a
signed `deposit.credited` webhook. `Fulfillment` does what Phala Cloud's backend does with it:

1. verify the Standard Webhooks `v1a` signature against the account's webhook keys pinned from
   attestation, over the raw body, and that the event names the product's account and mode;
2. credit once per deposit: the order row keyed by `provider_order_id`, the deposit's `dep_` id,
   is found-or-created under a unique index, and the credit transaction and
   `complete_order_payment` commit in the same transaction;
3. hold instead of crediting when the product refuses (unknown or suspended workspace, its own
   per-deposit or per-period cap); support later collects a refund address and requests a refund;
4. answer `2xx` only after that commit, so a failure is retried by the service.

Every other event type is stored for notifications and history; none moves a balance.
"""

from __future__ import annotations

import json
import logging
import time
import uuid
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from http import HTTPStatus
from typing import Any

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_sdk import (
    CREDITED_EVENT,
    AttestationError,
    CreditedDeposit,
    FulfillmentError,
    SignatureError,
    verify_webhook,
)

from .config import MissingProductKeyError, ProductConfig
from .ledger import ORDER_FLOW_CODE, ORDER_PROVIDER, ProductLedger

LOG = logging.getLogger(__name__)


class TransientError(Exception):
    """A dependency is unavailable; the operation can be retried."""


@dataclass(frozen=True)
class PinnedKeys:
    """The account's webhook keys in one mode, current first."""

    livemode: bool
    keys: Sequence[Ed25519PublicKey]


@dataclass
class Answer:
    status: int
    body: dict[str, Any] | None = None


def parse_decimal(value: object) -> int | None:
    if not isinstance(value, str) or not (value.isascii() and value.isdigit()):
        return None
    return int(value)


class Fulfillment:
    """Verifies deliveries, stores every event once, and credits `deposit.credited` once."""

    def __init__(
        self,
        config: ProductConfig,
        ledger: ProductLedger,
        webhook_keys: Callable[[], PinnedKeys],
    ) -> None:
        """`webhook_keys` returns the pinned keys, pinning them on first use; it raises
        `TransientError` or `MissingProductKeyError` while they cannot be pinned yet."""
        self.config = config
        self.ledger = ledger
        self.webhook_keys = webhook_keys

    def handle(self, headers: Mapping[str, str], body: bytes) -> Answer:
        try:
            pinned = self.webhook_keys()
        except (TransientError, MissingProductKeyError, AttestationError) as error:
            # The service retries: the keys are pinned once the product key is configured.
            LOG.warning("webhook keys are not pinned yet: %s", type(error).__name__)
            return Answer(HTTPStatus.SERVICE_UNAVAILABLE)
        try:
            event = verify_webhook(
                headers,
                body,
                pinned.keys,
                expected_account=self.config.product_slug,
                expected_livemode=pinned.livemode,
            )
        except SignatureError:
            return Answer(HTTPStatus.BAD_REQUEST)
        if event.type == CREDITED_EVENT:
            try:
                credit = CreditedDeposit.from_event(event)
            except FulfillmentError as error:
                LOG.warning("ignoring deposit.credited %s: %s", event.id, error)
            else:
                self.fulfill(credit)
        if self.ledger.record_event(event.id, event.type, event.data):
            LOG.info("webhook %s %s", event.type, (event.object or {}).get("id", ""))
        return Answer(HTTPStatus.NO_CONTENT)

    def fulfill(self, credit: CreditedDeposit) -> str:
        """Credits the deposit once and returns its order status (`accepted` or `held`)."""
        key = credit.fulfillment_key
        now = time.time()
        # SQLite's BEGIN IMMEDIATE serializes every writer. On PostgreSQL, lock the team row
        # (SELECT ... FOR UPDATE) before the per-period cap sum so concurrent credits cannot
        # both pass it, and keep provider_order_id unique across teams for this flow.
        with self.ledger.transaction() as db:
            existing = ProductLedger._find_order(db, key)
            if existing is not None:
                stored = parse_decimal(existing.payload.get("amount_minor"))
                if stored is not None and stored != credit.amount:
                    # Only a service restored from backup re-prices a spot deposit: keep the first
                    # credit and raise it with the operator.
                    LOG.error(
                        "deposit.credited %s repeats with %s minor, first credited %s",
                        key,
                        credit.amount,
                        stored,
                    )
                return existing.status
            hold = self._hold_reason(db, credit, now)
            order_id = str(uuid.uuid4())
            team_id = None if hold == "unknown_account" else credit.account_id
            db.execute(
                "INSERT INTO orders (id, team_id, provider, order_flow_code, provider_order_id, "
                "payload, status, reason, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    order_id,
                    team_id,
                    ORDER_PROVIDER,
                    ORDER_FLOW_CODE,
                    key,
                    json.dumps(_payload(credit)),
                    "held" if hold else "pending",
                    hold,
                    now,
                ),
            )
            if hold is None:
                credit_id = f"ctx_{uuid.uuid4().hex}"
                db.execute(
                    "INSERT INTO credit_transactions "
                    "(id, team_id, order_id, amount_minor, funding_source, created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?)",
                    (
                        credit_id,
                        credit.account_id,
                        order_id,
                        credit.amount,
                        f"crypto:{self.config.token_symbol}:{credit.chain_id}",
                        now,
                    ),
                )
                # complete_order_payment, in the same transaction as the credit.
                db.execute(
                    "UPDATE orders SET status = 'accepted', credit_transaction_id = ? WHERE id = ?",
                    (credit_id, order_id),
                )
        status = "held" if hold else "accepted"
        LOG.info("fulfillment %s -> %s %s", key, status, hold or "")
        return status

    def _hold_reason(self, db: Any, credit: CreditedDeposit, now: float) -> str | None:
        row = db.execute(
            "SELECT suspended FROM teams WHERE id = ?", (credit.account_id,)
        ).fetchone()
        if row is None:
            return "unknown_account"
        if row[0]:
            return "account_suspended"
        if credit.amount > self.config.per_deposit_cap_minor:
            return "per_deposit_cap"
        since = now - self.config.period_seconds
        already = self.ledger.credited_since(db, credit.account_id, since)
        if already + credit.amount > self.config.per_period_cap_minor:
            return "per_period_cap"
        return None


def _payload(credit: CreditedDeposit) -> dict[str, Any]:
    return {
        "deposit_id": credit.deposit_id,
        "account_id": credit.account_id,
        "amount_minor": str(credit.amount),
        "price_source": credit.price_source,
        "quote": credit.quote,
        "tx_hash": credit.tx_hash,
        "log_index": credit.log_index,
    }
