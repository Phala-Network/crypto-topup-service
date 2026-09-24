"""Python SDK for the crypto top-up service product API.

`topup_client` is generated from the service's OpenAPI document; this package adds request
signing, inbound verification, deterministic address helpers, and an idempotent client.
"""

from .addresses import deposit_id, forwarder_address, lock_salt, persistent_salt
from .attestation import attestation_report_data, verify_attestation_binding
from .client import TopupClient
from .errors import ApiError, AttestationError, SignatureError, TopupError
from .signing import RequestSigner, SigningAuth, VerifiedRequest, load_public_key, verify_request
from .webhooks import WebhookEvent, verify_webhook, verify_webhook_signature

__all__ = [
    "ApiError",
    "AttestationError",
    "RequestSigner",
    "SignatureError",
    "SigningAuth",
    "TopupClient",
    "TopupError",
    "VerifiedRequest",
    "WebhookEvent",
    "attestation_report_data",
    "deposit_id",
    "forwarder_address",
    "load_public_key",
    "lock_salt",
    "persistent_salt",
    "verify_attestation_binding",
    "verify_request",
    "verify_webhook",
    "verify_webhook_signature",
]
