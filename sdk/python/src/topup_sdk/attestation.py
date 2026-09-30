"""Verification of the `GET /v1/attestation` report-data binding (design D11).

The service signs each account's webhooks with that account's key in each mode, derived in the
attested CVM. `GET /v1/attestation`, called with the account's API key, returns the keys and TDX
evidence whose report data binds a fresh nonce, the account, the mode, and every listed key.
"""

from __future__ import annotations

import hashlib
from collections.abc import Sequence

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

from topup_client.models import AttestationResponse

from .errors import AttestationError
from .webhooks import webhook_public_key_bytes


def attestation_report_data(
    nonce: bytes, account: str, livemode: bool, keys: Sequence[tuple[int, bytes]]
) -> bytes:
    """Returns `sha256(len(nonce) ‖ nonce ‖ len(account) ‖ account ‖ livemode ‖ (version ‖
    public_key)*)`: one-byte lengths, the UTF-8 `acct_` id, one byte `1` live or `0` test, and
    for each `(version, raw 32-byte public key)` in order its version as 4 big-endian bytes and
    the key."""
    account_bytes = account.encode()
    if len(nonce) > 255 or len(account_bytes) > 255:
        raise ValueError("nonce and account must each be at most 255 bytes")
    content = bytes([len(nonce)]) + nonce + bytes([len(account_bytes)]) + account_bytes
    content += bytes([1 if livemode else 0])
    for version, public_key in keys:
        content += version.to_bytes(4, "big") + public_key
    return hashlib.sha256(content).digest()


def verify_attestation_binding(
    response: AttestationResponse,
    nonce: bytes,
    *,
    expected_account: str | None = None,
    expected_livemode: bool | None = None,
) -> list[Ed25519PublicKey]:
    """Checks that `report_data` binds `nonce`, the response's account and mode, and every listed
    webhook key, and that the account and mode are the expected ones when given; returns the
    public keys, current first. Each key's `public_key` is Standard Webhooks' `whpk_` form of the
    raw key `report_data` binds.

    This does not verify the quote itself: run the dstack verifier (`deploy/dstack-verifier.sh`)
    and confirm that the verified report data is `response.report_data` zero-padded to 64 bytes
    before pinning the keys.
    """
    if expected_account is not None and response.account != expected_account:
        raise AttestationError("attestation is for another account")
    if expected_livemode is not None and response.livemode != expected_livemode:
        raise AttestationError("attestation is for the other mode")
    if not response.webhook_keys:
        raise AttestationError("attestation lists no webhook key")
    keys: list[tuple[int, bytes]] = []
    try:
        for key in response.webhook_keys:
            keys.append((key.version, webhook_public_key_bytes(key.public_key)))
    except ValueError as error:
        raise AttestationError("attestation lists a malformed webhook key") from error
    if any(not 1 <= version < 2**32 for version, _ in keys):
        raise AttestationError("attestation lists a malformed webhook key")
    try:
        report_data = bytes.fromhex(response.report_data)
    except ValueError as error:
        raise AttestationError("attestation report data is not hexadecimal") from error
    try:
        expected = attestation_report_data(nonce, response.account, response.livemode, keys)
    except ValueError as error:
        raise AttestationError("attestation account is malformed") from error
    if report_data != expected:
        raise AttestationError("attestation report data does not bind the returned keys")
    return [Ed25519PublicKey.from_public_bytes(public_key) for _, public_key in keys]
