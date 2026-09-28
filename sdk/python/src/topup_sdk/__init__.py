"""Python SDK for the Phala Pay service product API.

`topup_client` is generated from the service's OpenAPI document; this package adds request
signing, inbound verification, deterministic address helpers, and an idempotent client.
"""

from .addresses import (
    deposit_address,
    deposit_address_salt,
    deposit_id,
    forwarder_address,
    lock_salt,
)
from .attestation import attestation_report_data, verify_attestation_binding
from .client import TopupClient
from .errors import AddressMismatchError, ApiError, AttestationError, SignatureError, TopupError
from .fulfillment import (
    CREDITED_EVENT,
    CreditedDeposit,
    FulfillmentError,
    credited_event_id,
)
from .signing import RequestSigner, SigningAuth, VerifiedRequest, load_public_key, verify_request
from .webhooks import WebhookEvent, sign_webhook, verify_webhook, verify_webhook_signature

__all__ = [
    "CREDITED_EVENT",
    "AddressMismatchError",
    "ApiError",
    "AttestationError",
    "CreditedDeposit",
    "FulfillmentError",
    "RequestSigner",
    "SignatureError",
    "SigningAuth",
    "TopupClient",
    "TopupError",
    "VerifiedRequest",
    "WebhookEvent",
    "attestation_report_data",
    "credited_event_id",
    "deposit_address",
    "deposit_address_salt",
    "deposit_id",
    "forwarder_address",
    "load_public_key",
    "lock_salt",
    "sign_webhook",
    "verify_attestation_binding",
    "verify_request",
    "verify_webhook",
    "verify_webhook_signature",
]
