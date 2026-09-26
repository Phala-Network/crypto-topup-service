"""Typed `deposit.credited` events for product fulfillment.

`deposit.credited` is the fulfillment event: the deposit is final, priced, and screened, and the
service owes the product `amount_minor` for `external_id`. A product credits each deposit once,
keyed by `fulfillment_key` under a unique index, and answers `2xx` after that commit:

    event = verify_webhook(headers, raw_body, settlement_key)
    if event.type == CREDITED_EVENT:
        fulfill(CreditedDeposit.from_event(event))

A repeated event for a deposit already fulfilled is a no-op. Its `amount_minor` can differ from
the stored credit only after the service was restored from a backup and re-priced a spot
deposit; keep the first credit and alert on the difference.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import Any, Literal

from .addresses import DEPOSIT_NAMESPACE
from .errors import TopupError
from .webhooks import WebhookEvent

CREDITED_EVENT = "deposit.credited"


def credited_event_id(deposit_id: uuid.UUID) -> uuid.UUID:
    """The `webhook-id` of a deposit's `deposit.credited`: UUIDv5(NS, `deposit.credited:<id>`).

    Derived from the deposit id, so every retry, replay, and re-emission after a service restore
    carries the same id.
    """
    return uuid.uuid5(DEPOSIT_NAMESPACE, f"{CREDITED_EVENT}:{deposit_id}")


class FulfillmentError(TopupError):
    """A verified event is not a well-formed `deposit.credited` event."""


@dataclass(frozen=True)
class CreditedDeposit:
    """The credit carried by one `deposit.credited` event."""

    event_id: str
    deposit_id: uuid.UUID
    product_id: uuid.UUID
    external_id: str
    unit: str
    amount_minor: int
    price_source: Literal["spot", "lock"]
    price_scaled: int
    price_scale: int
    valuation_at: str
    product_lock_ref: str | None
    address: str
    route: str
    route_version: int
    chain_id: int
    asset_contract: str
    tx_hash: str
    log_index: int
    amount_atomic: int

    @property
    def fulfillment_key(self) -> str:
        """The product's idempotency key for this credit: `deposit:<deposit_id>`."""
        return f"deposit:{self.deposit_id}"

    @classmethod
    def from_event(cls, event: WebhookEvent) -> CreditedDeposit:
        """Parses a verified event; raises `FulfillmentError` for any other shape.

        Events written before `deposit.credited` became the fulfillment event carry no
        `external_id` and are refused here; acknowledge them without crediting.
        """
        if event.type != CREDITED_EVENT:
            raise FulfillmentError(f"expected {CREDITED_EVENT}, got {event.type}")
        data = event.data
        try:
            price_source = _string(data, "price_source")
            if price_source not in ("spot", "lock"):
                raise FulfillmentError("price_source must be spot or lock")
            if data.get("state") != "credited":
                raise FulfillmentError("state must be credited")
            lock_ref = data.get("product_lock_ref")
            if lock_ref is not None and not isinstance(lock_ref, str):
                raise FulfillmentError("product_lock_ref must be a string or null")
            return cls(
                event_id=event.id,
                deposit_id=uuid.UUID(_string(data, "deposit_id")),
                product_id=uuid.UUID(_string(data, "product_id")),
                external_id=_string(data, "external_id"),
                unit=_string(data, "unit"),
                amount_minor=_decimal(data, "amount_minor"),
                price_source="lock" if price_source == "lock" else "spot",
                price_scaled=_decimal(data, "price_scaled"),
                price_scale=_integer(data, "price_scale"),
                valuation_at=_string(data, "valuation_at"),
                product_lock_ref=lock_ref,
                address=_string(data, "address"),
                route=_string(data, "route"),
                route_version=_integer(data, "route_version"),
                chain_id=_integer(data, "chain_id"),
                asset_contract=_string(data, "asset_contract"),
                tx_hash=_string(data, "tx_hash"),
                log_index=_integer(data, "log_index"),
                amount_atomic=_decimal(data, "amount_atomic"),
            )
        except ValueError as error:
            raise FulfillmentError(f"malformed {CREDITED_EVENT}: {error}") from error


def _string(data: dict[str, Any], name: str) -> str:
    value = data.get(name)
    if not isinstance(value, str) or not value:
        raise FulfillmentError(f"{name} must be a non-empty string")
    return value


def _decimal(data: dict[str, Any], name: str) -> int:
    value = _string(data, name)
    if not (value.isascii() and value.isdigit()):
        raise FulfillmentError(f"{name} must be a decimal string")
    return int(value)


def _integer(data: dict[str, Any], name: str) -> int:
    value = data.get(name)
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise FulfillmentError(f"{name} must be a non-negative integer")
    return value
