from __future__ import annotations

import json
from typing import Any

import pytest

from tests import vectors
from topup_sdk import RequestSigner, SignatureError, load_public_key, verify_request
from topup_sdk.signing import signature_params

SIGNER_KEY = load_public_key(RequestSigner.from_seed("x", vectors.SEED).public_key_base64())


def _committed() -> dict[str, Any]:
    result: dict[str, Any] = json.loads(vectors.VECTORS_PATH.read_text(encoding="utf-8"))
    return result


def test_signer_reproduces_the_vectors_the_rust_verifier_checks() -> None:
    # crates/topup/src/api/auth.rs verifies this committed file with the production verifier.
    assert vectors.build_vectors() == _committed()


def _verify(vector: dict[str, Any], **overrides: Any) -> None:
    arguments: dict[str, Any] = {
        "method": vector["method"],
        "target_uri": vector["target_uri"],
        "headers": vector["headers"],
        "body": vector["body"].encode(),
        "public_key": SIGNER_KEY,
        "keyid": vectors.KEYID,
        "require_idempotency_key": "idempotency-key" in vector["headers"],
        "now": vectors.CREATED,
    }
    arguments.update(overrides)
    verify_request(**arguments)


@pytest.mark.parametrize("vector", _committed()["vectors"], ids=lambda vector: vector["name"])
def test_verifier_accepts_the_profile(vector: dict[str, Any]) -> None:
    _verify(vector)


def _covered_vector() -> dict[str, Any]:
    vector: dict[str, Any] = next(
        vector for vector in _committed()["vectors"] if vector["name"] == "idempotency_key_covered"
    )
    return vector


@pytest.mark.parametrize(
    ("overrides", "header_changes"),
    [
        ({"now": vectors.CREATED + 301}, {}),
        ({"now": vectors.CREATED - 301}, {}),
        ({"keyid": "other/v1"}, {}),
        ({"target_uri": "http://127.0.0.1:18080/v1/products/other/accounts"}, {}),
        ({"method": "PUT"}, {}),
        ({"body": b"{}"}, {}),
        ({}, {"idempotency-key": '"checkout-2"'}),
        ({}, {"signature": "sig1=:AAAA:"}),
        ({}, {"signature-input": "sig1=()"}),
        ({"public_key": load_public_key("11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=")}, {}),
    ],
)
def test_verifier_rejects_mismatches(
    overrides: dict[str, Any], header_changes: dict[str, str]
) -> None:
    vector = _covered_vector()
    headers = {**vector["headers"], **header_changes}
    with pytest.raises(SignatureError):
        _verify(vector, headers=headers, **overrides)


def test_verifier_requires_idempotency_key_coverage_when_the_header_is_present() -> None:
    vector = next(
        vector for vector in _committed()["vectors"] if vector["name"] == "register_account"
    )
    headers = {**vector["headers"], "idempotency-key": '"deposit:x"'}
    with pytest.raises(SignatureError):
        _verify(vector, headers=headers)
    with pytest.raises(SignatureError):
        _verify(vector, require_idempotency_key=True)


def test_verifier_returns_the_unquoted_idempotency_key() -> None:
    vector = _covered_vector()
    verified = verify_request(
        method=vector["method"],
        target_uri=vector["target_uri"],
        headers=vector["headers"],
        body=vector["body"].encode(),
        public_key=SIGNER_KEY,
        keyid=vectors.KEYID,
        require_idempotency_key=True,
        now=vectors.CREATED,
    )
    assert verified.idempotency_key == "checkout-1"
    assert verified.created == vectors.CREATED


def test_keyid_is_serialized_as_an_escaped_structured_field_string() -> None:
    assert signature_params('a"b\\c', 1, cover_idempotency_key=False, include_alg=False) == (
        '("@method" "@target-uri" "content-digest");created=1;keyid="a\\"b\\\\c"'
    )
    with pytest.raises(ValueError, match="printable ASCII"):
        signature_params("clé", 1, cover_idempotency_key=False)


def test_seed_must_be_32_bytes() -> None:
    with pytest.raises(ValueError, match="32 bytes"):
        RequestSigner.from_seed("x", b"short")
