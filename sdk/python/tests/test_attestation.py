from __future__ import annotations

import hashlib

import httpx
import pytest

from topup_client.models import AttestationResponse, OperatorIdentity
from topup_sdk import (
    AttestationError,
    RequestSigner,
    TopupClient,
    attestation_report_data,
    verify_attestation_binding,
)

NONCE = bytes(range(16))
SETTLEMENT = bytes([0x42] * 32)
# The known vector of `report_data_binds_operators_in_canonical_order` in
# crates/adapters/src/attestation.rs.
OPERATORS = [
    {"chain_id": 1, "operator_key_version": 2, "keyid": "operator/v2", "address": "0x" + "11" * 20},
    {
        "chain_id": 11_155_111,
        "operator_key_version": 1,
        "keyid": "operator/v1",
        "address": "0x" + "22" * 20,
    },
]
REPORT_DATA = "c30486f4d5a70ddf9a44157ce18f8c1e37a9479f15c02b80c4e373dc639fa9ea"


def _response(**overrides: object) -> dict[str, object]:
    body: dict[str, object] = {
        "keyid": "settlement/v1",
        "settlement_pubkey": SETTLEMENT.hex(),
        "operators": OPERATORS,
        "report_data": REPORT_DATA,
        "quote": "",
    }
    body.update(overrides)
    return body


def test_report_data_matches_the_rust_vector() -> None:
    operators = [OperatorIdentity.from_dict(operator) for operator in OPERATORS]
    assert attestation_report_data(NONCE, SETTLEMENT, operators).hex() == REPORT_DATA


def test_a_response_without_operators_uses_the_original_binding() -> None:
    body = _response(report_data=hashlib.sha256(NONCE + SETTLEMENT).hexdigest())
    del body["operators"]
    verify_attestation_binding(AttestationResponse.from_dict(body), NONCE)


@pytest.mark.parametrize(
    "overrides",
    [
        {"operators": [OPERATORS[0], {**OPERATORS[1], "address": "0x" + "33" * 20}]},
        {"operators": OPERATORS[:1]},
        {"operators": list(reversed(OPERATORS))},
        {"operators": [OPERATORS[0], {**OPERATORS[1], "keyid": "operator/v2"}]},
        {"operators": [OPERATORS[0], {**OPERATORS[1], "address": "0x" + "2" * 40 + "A"}]},
        {"settlement_pubkey": "43" * 32},
    ],
)
def test_bindings_that_do_not_match_are_rejected(overrides: dict[str, object]) -> None:
    with pytest.raises(AttestationError):
        verify_attestation_binding(AttestationResponse.from_dict(_response(**overrides)), NONCE)


def test_client_attestation_verifies_the_binding() -> None:
    bodies = [_response(), _response(operators=OPERATORS[1:])]

    def respond(request: httpx.Request) -> httpx.Response:
        assert request.url.params["nonce"] == NONCE.hex()
        return httpx.Response(200, json=bodies.pop(0))

    signer = RequestSigner.from_seed("acme/v1", bytes([5] * 32))
    with TopupClient(
        "http://service.test:8080", "acme", signer, transport=httpx.MockTransport(respond)
    ) as client:
        evidence = client.attestation(NONCE)
        assert isinstance(evidence.operators, list)
        assert [operator.address for operator in evidence.operators] == [
            OPERATORS[0]["address"],
            OPERATORS[1]["address"],
        ]
        with pytest.raises(AttestationError):
            client.attestation(NONCE)
