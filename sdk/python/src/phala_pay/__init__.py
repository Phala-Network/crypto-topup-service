"""Phala Pay for Python, in the shape of Stripe's.

    import os

    from phala_pay import PhalaPay

    pay = PhalaPay(api_base="https://pay.example.com", api_key=os.environ["PHALA_PAY_KEY"])
    quote = pay.quotes.create(account_id="team-42", amount=2500, chain_id=11155111, asset="pha")

    event = pay.webhooks.construct_event(raw_body, request.headers, SETTLEMENT_PUBLIC_KEY)
    if event.type == "deposit.credited":
        credit_once(event.deposit.id, event.deposit.account_id, event.deposit.amount)

`topup_sdk` holds the lower-level pieces (address derivation, attestation, webhook and admin
request signatures) and `topup_client` the client generated from the OpenAPI document.
"""

from topup_client.models import ClientQuote, Config, Deposit, Quote, Refund
from topup_sdk import AddressMismatchError, ApiError, TopupError

from ._client import PhalaPay
from ._webhook import Event, EventData, SignatureVerificationError, Webhook

__all__ = [
    "AddressMismatchError",
    "ApiError",
    "ClientQuote",
    "Config",
    "Deposit",
    "Event",
    "EventData",
    "PhalaPay",
    "Quote",
    "Refund",
    "SignatureVerificationError",
    "TopupError",
    "Webhook",
]
