"""Phala Pay for Python, in the shape of Stripe's.

    import os

    from phala_pay import PhalaPay

    pay = PhalaPay(
        api_base="https://pay.example.com",
        api_key=os.environ["PHALA_PAY_KEY"],
        forwarder=(FACTORY, IMPLEMENTATION),
    )
    quote = pay.quotes.create(
        client_reference_id="team-42", amount=2500, chain_id=11155111, asset="pha"
    )

    event = pay.webhooks.construct_event(
        raw_body, request.headers, WEBHOOK_PUBLIC_KEY, "acct_…", expected_livemode=False
    )
    if event.type == "deposit.credited":
        credit_once(event.deposit.id, event.deposit.client_reference_id, event.deposit.amount)

`topup_sdk` holds the lower-level pieces (address derivation, attestation, webhook and admin
request signatures, offline `flush_transaction` and `safe_batch`, `export_account`) and
`topup_client` the client generated from the OpenAPI document.
"""

from topup_client.models import (
    Balance,
    ClientQuote,
    Config,
    Deposit,
    DepositAddress,
    Forwarder,
    Quote,
    Refund,
    Sweep,
    Treasury,
)
from topup_sdk import (
    AddressMismatchError,
    ApiError,
    TopupError,
    flush_transaction,
    flush_transactions,
    safe_batch,
    write_safe_batch,
)

from ._client import PhalaPay
from ._types import (
    ApiKeyStatus,
    DepositAddressStatus,
    DepositStatus,
    EventType,
    PaymentStatus,
    QuoteStatus,
    RefundStatus,
    TreasuryStatus,
    WebhookEndpointStatus,
)
from ._webhook import Event, EventData, EventRequest, SignatureVerificationError, Webhook

__all__ = [
    "AddressMismatchError",
    "ApiError",
    "ApiKeyStatus",
    "Balance",
    "ClientQuote",
    "Config",
    "Deposit",
    "DepositAddress",
    "DepositAddressStatus",
    "DepositStatus",
    "Event",
    "EventData",
    "EventRequest",
    "EventType",
    "Forwarder",
    "PaymentStatus",
    "PhalaPay",
    "Quote",
    "QuoteStatus",
    "Refund",
    "RefundStatus",
    "SignatureVerificationError",
    "Sweep",
    "TopupError",
    "Treasury",
    "TreasuryStatus",
    "Webhook",
    "WebhookEndpointStatus",
    "flush_transaction",
    "flush_transactions",
    "safe_batch",
    "write_safe_batch",
]
