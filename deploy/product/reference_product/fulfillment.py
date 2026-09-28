"""The webhook receiver and fulfillment: the product side of `deposit.*` events.

The service credits a deposit once it is priced and screened, and tells the product with a signed
`deposit.credited` webhook. `Fulfillment` does what Phala Cloud's backend does with it:

1. verify the Standard Webhooks `v1a` signature against the account's webhook keys pinned from
   attestation, over the raw body, and that the event names the product's account and mode;
2. credit once per deposit: the order row keyed by `provider_order_id`, the deposit's `dep_` id,
   is found-or-created under a unique index, and the credit transaction and
   `complete_order_payment` commit in the same transaction;
3. hold instead of crediting when the product refuses (unknown or suspended workspace, its own
   per-deposit or per-period cap); support later collects a refund address and requests a refund;
4. answer `2xx` only after that commit, so a failure is retried by the service.

`deposit.refunded` and `deposit.reversed` take a credit back. Every `deposit.*` event carries the
whole deposit with cumulative, service-computed claw-backs (`amount_refunded`, the refunded share
of the credit; `amount_reversed`, all of it once reversed), and events may arrive in any order, so
the balance follows the snapshots, not the event types: per deposit, in one transaction, the stored
view and the snapshot merge (the later status and the larger claw-backs win), and a credited order
is adjusted to what its credit nets to, `credit - amount_refunded - amount_reversed`. A
`deposit.reversed` that arrives before `deposit.credited` leaves nothing to credit. Every other
event type is stored for notifications and history; none moves a balance.
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
from .ledger import ORDER_FLOW_CODE, ORDER_PROVIDER, DepositView, ProductLedger

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


def deposit_view(deposit: Mapping[str, Any]) -> tuple[str, DepositView] | None:
    """A `deposit.*` event's snapshot: its `dep_` id and view, or `None` when malformed."""
    fields = (
        deposit.get("id"),
        deposit.get("status"),
        deposit.get("amount_refunded"),
        deposit.get("amount_reversed"),
    )
    match fields:
        case (str(key), str(status), int(refunded), int(reversed_)) if (
            refunded >= 0 and reversed_ >= 0
        ):
            return key, DepositView(status, refunded, reversed_)
    return None


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
                expected_account=self.config.account,
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
                self.fulfill(credit, event.object or {})
        elif event.type.startswith("deposit."):
            snapshot = deposit_view(event.object or {})
            if snapshot is None:
                LOG.warning("ignoring %s %s: malformed deposit", event.type, event.id)
            else:
                self.settle(*snapshot, reason=event.type)
        if self.ledger.record_event(event.id, event.type, event.data):
            LOG.info("webhook %s %s", event.type, (event.object or {}).get("id", ""))
        return Answer(HTTPStatus.NO_CONTENT)

    def fulfill(self, credit: CreditedDeposit, deposit: Mapping[str, Any] | None = None) -> str:
        """Credits the deposit once and returns its order status (`accepted` or `held`), or
        `reversed` when an earlier snapshot showed the deposit reversed, leaving nothing to credit.
        `deposit` is the event's deposit object, whose claw-backs apply to the credit at once."""
        key = credit.fulfillment_key
        view = deposit_view(deposit or {}) or (key, DepositView("credited", 0, 0))
        return self.settle(key, view[1], credit=credit, reason=CREDITED_EVENT) or "reversed"

    def settle(
        self,
        key: str,
        snapshot: DepositView,
        *,
        credit: CreditedDeposit | None = None,
        reason: str,
    ) -> str | None:
        """Merges a deposit snapshot and adjusts its credited order to what the credit nets to;
        with `credit`, first credits the deposit when it has no order. Returns the order status,
        or `None` when the deposit has no order."""
        now = time.time()
        # SQLite's BEGIN IMMEDIATE serializes every writer, so deliveries of one deposit apply one
        # at a time. On PostgreSQL, lock the deposit's snapshot row and the team row
        # (SELECT ... FOR UPDATE) before the per-period cap sum so concurrent credits cannot
        # both pass it, and keep provider_order_id unique across teams for this flow.
        with self.ledger.transaction() as db:
            view = ProductLedger.merge_snapshot(db, key, snapshot)
            order = ProductLedger._find_order(db, key)
            if order is None:
                if credit is None or view.status == "reversed":
                    return None
                order_id, status = self._credit(db, credit, now)
            else:
                order_id, status = order.id, order.status
                stored = parse_decimal(order.payload.get("amount_minor"))
                if credit is not None and stored is not None and stored != credit.amount:
                    # Only a service restored from backup re-prices a spot deposit: keep the first
                    # credit and raise it with the operator.
                    LOG.error(
                        "deposit.credited %s repeats with %s minor, first credited %s",
                        key,
                        credit.amount,
                        stored,
                    )
            if status == "accepted":
                credited, net = ProductLedger.order_amounts(db, order_id)
                change = view.contribution(credited) - net
                if change:
                    db.execute(
                        "INSERT INTO credit_adjustments "
                        "(id, team_id, order_id, amount_minor, reason, created_at) "
                        "SELECT ?, team_id, id, ?, ?, ? FROM orders WHERE id = ?",
                        (f"adj_{uuid.uuid4().hex}", change, reason, now, order_id),
                    )
                    LOG.info("fulfillment %s adjusted %+d by %s", key, change, reason)
        return status

    def _credit(self, db: Any, credit: CreditedDeposit, now: float) -> tuple[str, str]:
        """Creates the deposit's order, credited or held; returns its id and status."""
        key = credit.fulfillment_key
        hold = self._hold_reason(db, credit, now)
        order_id = str(uuid.uuid4())
        team_id = None if hold == "unknown_account" else credit.client_reference_id
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
                    credit.client_reference_id,
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
        return order_id, status

    def _hold_reason(self, db: Any, credit: CreditedDeposit, now: float) -> str | None:
        row = db.execute(
            "SELECT suspended FROM teams WHERE id = ?", (credit.client_reference_id,)
        ).fetchone()
        if row is None:
            return "unknown_account"
        if row[0]:
            return "account_suspended"
        if credit.amount > self.config.per_deposit_cap_minor:
            return "per_deposit_cap"
        since = now - self.config.period_seconds
        already = self.ledger.credited_since(db, credit.client_reference_id, since)
        if already + credit.amount > self.config.per_period_cap_minor:
            return "per_period_cap"
        return None


def _payload(credit: CreditedDeposit) -> dict[str, Any]:
    return {
        "deposit_id": credit.deposit_id,
        "account_id": credit.client_reference_id,
        "amount_minor": str(credit.amount),
        "price_source": credit.price_source,
        "quote": credit.quote,
        "tx_hash": credit.tx_hash,
        "log_index": credit.log_index,
    }
