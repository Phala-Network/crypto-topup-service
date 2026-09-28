from __future__ import annotations

import httpx
import pytest

from topup_client.models import AttestationResponse
from topup_sdk import (
    AttestationError,
    TopupClient,
    attestation_report_data,
    verify_attestation_binding,
)

NONCE = bytes(range(16))
SETTLEMENT = bytes([0x42] * 32)
# The known vector of `report_data_matches_the_published_vector` in
# crates/adapters/src/attestation.rs.
REPORT_DATA = "58c4e8b13ba082a25854a52564151194a7ec3221acc8aa8884f2aba2dda1037f"


def _response(**overrides: object) -> dict[str, object]:
    body: dict[str, object] = {
        "keyid": "settlement/v1",
        "settlement_pubkey": SETTLEMENT.hex(),
        "report_data": REPORT_DATA,
        "quote": "",
    }
    body.update(overrides)
    return body


def test_report_data_matches_the_rust_vector() -> None:
    assert attestation_report_data(NONCE, SETTLEMENT).hex() == REPORT_DATA


@pytest.mark.parametrize(
    "overrides",
    [
        {"settlement_pubkey": "43" * 32},
        {"settlement_pubkey": "42" * 31},
        {"report_data": "00" * 32},
        {"report_data": "not hex"},
    ],
)
def test_bindings_that_do_not_match_are_rejected(overrides: dict[str, object]) -> None:
    with pytest.raises(AttestationError):
        verify_attestation_binding(AttestationResponse.from_dict(_response(**overrides)), NONCE)


def test_client_attestation_verifies_the_binding() -> None:
    bodies = [_response(), _response(settlement_pubkey="43" * 32)]

    def respond(request: httpx.Request) -> httpx.Response:
        assert request.url.params["nonce"] == NONCE.hex()
        return httpx.Response(200, json=bodies.pop(0))

    api_key = "ppay_sk_test_" + "C" * 43 + "000000"
    with TopupClient(
        "http://service.test:8080", api_key, transport=httpx.MockTransport(respond)
    ) as client:
        evidence = client.attestation(NONCE)
        assert evidence.settlement_pubkey == SETTLEMENT.hex()
        with pytest.raises(AttestationError):
            client.attestation(NONCE)
