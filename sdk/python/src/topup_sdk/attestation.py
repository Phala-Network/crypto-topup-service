"""Verification of the `GET /v1/attestation` report-data binding."""

from __future__ import annotations

import hashlib

from topup_client.models import AttestationResponse

from .errors import AttestationError


def attestation_report_data(nonce: bytes, settlement_pubkey: bytes) -> bytes:
    """Returns `sha256(nonce ‖ settlement_pubkey)`."""
    return hashlib.sha256(nonce + settlement_pubkey).digest()


def verify_attestation_binding(response: AttestationResponse, nonce: bytes) -> None:
    """Checks that `report_data` binds `nonce` and the settlement key.

    This does not verify the quote itself: run the dstack verifier (`deploy/dstack-verifier.sh`)
    and confirm that the verified report data is `response.report_data` zero-padded to 64 bytes
    before trusting any of these values.
    """
    try:
        settlement_pubkey = bytes.fromhex(response.settlement_pubkey)
        report_data = bytes.fromhex(response.report_data)
    except ValueError as error:
        raise AttestationError("attestation fields are not hexadecimal") from error
    if len(settlement_pubkey) != 32:
        raise AttestationError("attestation settlement key is not 32 bytes")
    if report_data != attestation_report_data(nonce, settlement_pubkey):
        raise AttestationError("attestation report data does not bind the returned key")
