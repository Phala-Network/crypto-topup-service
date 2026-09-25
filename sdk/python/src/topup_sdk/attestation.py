"""Verification of the `GET /v1/attestation` report-data binding."""

from __future__ import annotations

import hashlib
import re
from collections.abc import Sequence

from topup_client.models import AttestationResponse, OperatorIdentity
from topup_client.types import Unset

from .errors import AttestationError

_ADDRESS = re.compile(r"0x[0-9a-f]{40}")
_U64 = 2**64
_U32 = 2**32


def attestation_report_data(
    nonce: bytes, settlement_pubkey: bytes, operators: Sequence[OperatorIdentity]
) -> bytes:
    """Returns `sha256(nonce ‖ settlement_pubkey ‖ record_1 ‖ … ‖ record_n)`.

    Each operator contributes a 32-byte record, `chain_id` (u64 big-endian) ‖
    `operator_key_version` (u32 big-endian) ‖ the 20 address bytes, in the given order. Without
    operators this is `sha256(nonce ‖ settlement_pubkey)`.
    """
    digest = hashlib.sha256(nonce + settlement_pubkey)
    for operator in operators:
        if not 0 <= operator.chain_id < _U64 or not 0 < operator.operator_key_version < _U32:
            raise AttestationError("attestation operator is out of range")
        if not _ADDRESS.fullmatch(operator.address):
            raise AttestationError("attestation operator address is not lowercase hexadecimal")
        digest.update(operator.chain_id.to_bytes(8, "big"))
        digest.update(operator.operator_key_version.to_bytes(4, "big"))
        digest.update(bytes.fromhex(operator.address[2:]))
    return digest.digest()


def verify_attestation_binding(response: AttestationResponse, nonce: bytes) -> None:
    """Checks that `report_data` binds `nonce`, the settlement key, and every listed operator.

    This does not verify the quote itself: run the dstack verifier (`deploy/dstack-verifier.sh`)
    and confirm that the verified report data is `response.report_data` zero-padded to 64 bytes
    before trusting any of these values.
    """
    operators = [] if isinstance(response.operators, Unset) else response.operators
    try:
        settlement_pubkey = bytes.fromhex(response.settlement_pubkey)
        report_data = bytes.fromhex(response.report_data)
    except ValueError as error:
        raise AttestationError("attestation fields are not hexadecimal") from error
    if len(settlement_pubkey) != 32:
        raise AttestationError("attestation settlement key is not 32 bytes")
    chain_ids = [operator.chain_id for operator in operators]
    if chain_ids != sorted(set(chain_ids)):
        raise AttestationError("attestation operators are not one per chain in ascending order")
    for operator in operators:
        if operator.keyid != f"operator/v{operator.operator_key_version}":
            raise AttestationError("attestation operator keyid does not match its version")
    if report_data != attestation_report_data(nonce, settlement_pubkey, operators):
        raise AttestationError("attestation report data does not bind the returned keys")
