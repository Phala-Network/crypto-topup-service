"""Python SDK for the crypto top-up service, in the shape of Stripe's.

    from crypto_topup import CryptoTopup, Webhook

    client = CryptoTopup("https://topup.example.com", "acme/v1", key_file="product.seed")
    quote = client.quotes.create(account_id="team-42", amount=2500, chain_id=11155111, asset="pha")

    event = Webhook.construct_event(raw_body, request.headers, SETTLEMENT_PUBLIC_KEY)
    if event.type == "deposit.credited":
        credit_once(event.deposit.id, event.deposit.account_id, event.deposit.amount)

`topup_sdk` holds the lower-level pieces (request signing, address derivation, attestation) and
`topup_client` the client generated from the OpenAPI document.
"""

from topup_client.models import ClientQuote, Config, Deposit, Quote, Refund
from topup_sdk import AddressMismatchError, ApiError, TopupError

from ._client import CryptoTopup
from ._webhook import Event, EventData, SignatureVerificationError, Webhook

__all__ = [
    "AddressMismatchError",
    "ApiError",
    "ClientQuote",
    "Config",
    "CryptoTopup",
    "Deposit",
    "Event",
    "EventData",
    "Quote",
    "Refund",
    "SignatureVerificationError",
    "TopupError",
    "Webhook",
]
