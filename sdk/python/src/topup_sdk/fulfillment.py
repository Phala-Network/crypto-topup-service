"""Typed `deposit.credited` events for product fulfillment.

`deposit.credited` is the fulfillment event: the deposit is priced and screened, and the service
owes the product `amount` cents for `client_reference_id`. A product credits each deposit once,
keyed by `fulfillment_key`, the deposit's `dep_` id, under a unique index, and answers `2xx`
after that commit:

    event = verify_webhook(
        headers, raw_body, webhook_keys, expected_account="acct_…", expected_livemode=True
    )
    if event.type == CREDITED_EVENT:
        fulfill(CreditedDeposit.from_event(event))

A repeated event for a deposit already fulfilled is a no-op. Its `amount` can differ from the
stored credit only after the service was restored from a backup and re-priced a spot deposit;
keep the first credit and alert on the difference.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import Any, Literal

from .addresses import DEPOSIT_NAMESPACE
from .errors import TopupError
from .ids import DEPOSIT, EVENT, QUOTE, object_id, parse_id
from .webhooks import WebhookEvent

CREDITED_EVENT = "deposit.credited"


def credited_event_id(deposit_id: str) -> str:
    """The `webhook-id` of a deposit's `deposit.credited`: `evt_` and the hex of
    UUIDv5(NS, `deposit.credited:<deposit UUID>`).

    Derived from the `dep_` id, so every retry, replay, and re-emission after a service restore
    carries the same id.
    """
    deposit = parse_id(DEPOSIT, deposit_id)
    return object_id(EVENT, uuid.uuid5(DEPOSIT_NAMESPACE, f"{CREDITED_EVENT}:{deposit}"))


class FulfillmentError(TopupError):
    """A verified event is not a well-formed `deposit.credited` event."""


@dataclass(frozen=True)
class CreditedDeposit:
    """The credit carried by one `deposit.credited` event: fields of its `data.object`."""

    event_id: str
    deposit_id: str
    client_reference_id: str
    amount: int
    currency: str
    price_source: Literal["quote", "spot"]
    exchange_rate: str
    quote: str | None
    chain_id: int
    asset: str | None
    asset_contract: str
    amount_atomic: int
    address: str
    tx_hash: str
    log_index: int

    @property
    def fulfillment_key(self) -> str:
        """The product's idempotency key for this credit: the deposit's `dep_` id."""
        return self.deposit_id

    @classmethod
    def from_event(cls, event: WebhookEvent) -> CreditedDeposit:
        """Parses a verified event; raises `FulfillmentError` for any other shape."""
        if event.type != CREDITED_EVENT:
            raise FulfillmentError(f"expected {CREDITED_EVENT}, got {event.type}")
        deposit = event.object
        if deposit is None or deposit.get("object") != "deposit":
            raise FulfillmentError(f"{CREDITED_EVENT} carries no deposit object")
        try:
            if deposit.get("status") != "credited":
                raise FulfillmentError("status must be credited")
            price_source = _string(deposit, "price_source")
            if price_source not in ("quote", "spot"):
                raise FulfillmentError("price_source must be quote or spot")
            deposit_id = _string(deposit, "id")
            parse_id(DEPOSIT, deposit_id)
            quote = _quote_id(deposit.get("quote"))
            asset = deposit.get("asset")
            if asset is not None and not isinstance(asset, str):
                raise FulfillmentError("asset must be a string or null")
            return cls(
                event_id=event.id,
                deposit_id=deposit_id,
                client_reference_id=_string(deposit, "client_reference_id"),
                amount=_integer(deposit, "amount"),
                currency=_string(deposit, "currency"),
                price_source="quote" if price_source == "quote" else "spot",
                exchange_rate=_string(deposit, "exchange_rate"),
                quote=quote,
                chain_id=_integer(deposit, "chain_id"),
                asset=asset,
                asset_contract=_string(deposit, "asset_contract"),
                amount_atomic=_decimal(deposit, "amount_atomic"),
                address=_string(deposit, "address"),
                tx_hash=_string(deposit, "tx_hash"),
                log_index=_integer(deposit, "log_index"),
            )
        except ValueError as error:
            raise FulfillmentError(f"malformed {CREDITED_EVENT}: {error}") from error


def _quote_id(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, dict):
        value = value.get("id")
    if not isinstance(value, str):
        raise FulfillmentError("quote must be a qt_ id, an expanded quote, or null")
    parse_id(QUOTE, value)
    return value


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
