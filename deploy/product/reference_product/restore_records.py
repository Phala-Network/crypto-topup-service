"""The merchant's records a service restore asks for, from the product's ledger.

After the service is restored from backup, its operator asks every merchant for what it did or
received after the restore point (deploy/runbooks/restore.md, step 2). `export_restore_records`
answers with the product's own records, each already in the body of the admin request that brings
it back, without the operator's `reason`:

- `deposit_addresses`: `POST /v1/admin/restore/deposit_addresses` bodies (step 4), from each
  deposit address response the product recorded, with its `client_secret`;
- `quotes`: `POST /v1/admin/restore/quotes` bodies (step 4), from each quote response the product
  recorded, with its `client_secret`;
- `events`: `POST /v1/admin/restore/events` bodies (step 5) of at most 100 deliveries each, every
  `deposit.credited`, `deposit.rejected`, and `deposit.reversed` delivery the webhook inbox kept,
  its raw body and Standard Webhooks headers exactly as received.

With `since` (the restore point, Unix seconds), only records the service created from then on are
exported; without it, everything, which is safe: a record the restored service still holds is
answered `reissued: false` or `matches`. The export holds client secrets: hand it to the operator
over the incident's channel only, and delete it once the restore is done.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Mapping
from typing import Any

from .ledger import Delivery, ProductLedger

LOG = logging.getLogger(__name__)

# The event types a restore re-derives from the chain, so imports (restore_mode.rs).
RESTORED_EVENT_TYPES = ("deposit.credited", "deposit.rejected", "deposit.reversed")
# Deliveries per `POST /v1/admin/restore/events`.
MAX_DELIVERIES = 100
# The quote fields `POST /v1/admin/restore/quotes` takes, as `POST /v1/quotes` returned them.
QUOTE_FIELDS = (
    "livemode",
    "id",
    "client_reference_id",
    "chain_id",
    "asset",
    "amount",
    "amount_atomic",
    "exchange_rate",
    "address",
    "created",
    "expires_at",
    "metadata",
    "client_secret",
)


def export_restore_records(
    account: str, ledger: ProductLedger, *, since: int | None = None
) -> dict[str, Any]:
    """The product's restore records for `account`, as the operator's admin requests take them."""

    def after(created: object) -> bool:
        return since is None or not isinstance(created, int) or created >= since

    addresses = [
        deposit_address_request(account, response)
        for response in ledger.deposit_address_records()
        if after(response.get("created"))
    ]
    quotes = [
        quote_request(account, response)
        for response in ledger.quote_records()
        if after(response.get("created"))
    ]
    deliveries = []
    for delivery in ledger.deliveries(RESTORED_EVENT_TYPES):
        exported = delivery_request(delivery)
        if exported is not None and after(json.loads(exported["body"]).get("created")):
            deliveries.append(exported)
    missing = ledger.events_without_evidence(RESTORED_EVENT_TYPES)
    if missing:
        LOG.warning("%d events were stored before the inbox kept deliveries; not exported", missing)
    return {
        "account": account,
        "since": since,
        "deposit_addresses": addresses,
        "quotes": quotes,
        "events": [
            {"deliveries": deliveries[start : start + MAX_DELIVERIES]}
            for start in range(0, len(deliveries), MAX_DELIVERIES)
        ],
    }


def quote_request(account: str, quote: Mapping[str, Any]) -> dict[str, Any]:
    """A `POST /v1/admin/restore/quotes` body from a `POST /v1/quotes` response."""
    request = {"account": account}
    request.update({name: quote[name] for name in QUOTE_FIELDS if quote.get(name) is not None})
    return request


def deposit_address_request(account: str, address: Mapping[str, Any]) -> dict[str, Any]:
    """A `POST /v1/admin/restore/deposit_addresses` body from a deposit address response: its id
    and version, and its address (the top-level one, or its first network's)."""
    networks = address.get("networks") or []
    request = {
        "account": account,
        "livemode": address["livemode"],
        "client_reference_id": address["client_reference_id"],
        "id": address["id"],
        "version": address["version"],
        "address": address.get("address") or (networks[0]["address"] if networks else None),
        "client_secret": address.get("client_secret"),
    }
    return {name: value for name, value in request.items() if value is not None}


def delivery_request(delivery: Delivery) -> dict[str, str] | None:
    """A delivery as `POST /v1/admin/restore/events` takes it; `None` for a body that is not
    UTF-8, which no JSON string can carry byte for byte."""
    try:
        body = delivery.body.decode("utf-8")
    except UnicodeDecodeError:
        LOG.warning("webhook %s: the body is not UTF-8; not exported", delivery.webhook_id)
        return None
    return {
        "webhook_id": delivery.webhook_id,
        "webhook_timestamp": delivery.webhook_timestamp,
        "webhook_signature": delivery.webhook_signature,
        "body": body,
    }
