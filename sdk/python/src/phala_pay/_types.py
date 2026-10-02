"""`Literal` names of the API's statuses and event types, for type checkers.

The generated client types every status as `str`: a status or event type added later must not
break parsing. These hints name the values documented today; treat any other value as unknown.
"""

from __future__ import annotations

from typing import Literal

QuoteStatus = Literal["open", "complete", "expired", "canceled"]
DepositStatus = Literal["pending", "credited", "rejected", "reversed"]
DepositAddressStatus = Literal["active", "retired"]
RefundStatus = Literal["pending", "succeeded", "failed", "canceled"]
TreasuryStatus = Literal["pending", "active", "replaced", "canceled"]
ApiKeyStatus = Literal["active", "expiring", "expired", "revoked"]
WebhookEndpointStatus = Literal["enabled", "disabled"]
PaymentStatus = Literal["seen", "recorded"]
PaymentSettingsStatus = Literal["unconfigured", "configured", "held"]
RejectionReason = Literal[
    "unsupported_asset",
    "asset_not_accepted",
    "below_minimum",
    "out_of_bounds",
    "out_of_range",
    "sanctioned",
]

EventType = Literal[
    "account.updated",
    "api_key.created",
    "api_key.revoked",
    "api_key.updated",
    "deposit.credited",
    "deposit.refunded",
    "deposit.rejected",
    "deposit.reversed",
    "payment_settings.updated",
    "quote.canceled",
    "quote.expired",
    "refund.created",
    "refund.failed",
    "refund.updated",
    "treasury.canceled",
    "treasury.created",
    "treasury.updated",
    "webhook_endpoint.created",
    "webhook_endpoint.deleted",
    "webhook_endpoint.test",
    "webhook_endpoint.updated",
]
"""Every event type; `GET /v1/events?type=` also takes a group, such as `deposit.*`."""
