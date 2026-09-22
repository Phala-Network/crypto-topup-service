"""Python SDK for the crypto top-up service product API.

`topup_client` is generated from the service's OpenAPI document; this package adds request
signing, inbound verification, deterministic address helpers, and an idempotent client.
"""

from .addresses import deposit_id, forwarder_address, lock_salt, persistent_salt
from .client import TopupClient
from .errors import ApiError, SignatureError, TopupError
from .signing import RequestSigner, SigningAuth, VerifiedRequest, load_public_key, verify_request
from .webhooks import WebhookEvent, verify_webhook, verify_webhook_signature

__all__ = [
    "ApiError",
    "RequestSigner",
    "SignatureError",
    "SigningAuth",
    "TopupClient",
    "TopupError",
    "VerifiedRequest",
    "WebhookEvent",
    "deposit_id",
    "forwarder_address",
    "load_public_key",
    "lock_salt",
    "persistent_salt",
    "verify_request",
    "verify_webhook",
    "verify_webhook_signature",
]
