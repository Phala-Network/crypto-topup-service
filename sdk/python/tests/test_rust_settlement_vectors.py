"""Settlement requests signed by the Rust service verify with the Python SDK."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from topup_sdk import SignatureError, load_public_key, verify_request

# Produced and checked by crates/adapters/src/settlement/http.rs
# (settlement_signatures_match_the_cross_language_fixture).
FIXTURE: dict[str, Any] = json.loads(
    (
        Path(__file__).resolve().parents[3]
        / "crates/adapters/tests/fixtures/rfc9421-rust-settlement.json"
    ).read_text(encoding="utf-8")
)
KEY = load_public_key(FIXTURE["public_key"])


def _verify(vector: dict[str, Any], **overrides: Any) -> str | None:
    arguments: dict[str, Any] = {
        "method": vector["method"],
        "target_uri": vector["target_uri"],
        "headers": vector["headers"],
        "body": vector["body"].encode(),
        "public_key": KEY,
        "keyid": FIXTURE["keyid"],
        "require_idempotency_key": True,
        "now": FIXTURE["created"],
    }
    arguments.update(overrides)
    return verify_request(**arguments).idempotency_key


@pytest.mark.parametrize("vector", FIXTURE["vectors"], ids=lambda vector: vector["name"])
def test_rust_settlement_signature_verifies(vector: dict[str, Any]) -> None:
    assert _verify(vector) == "deposit:20513a59-9b80-53df-9832-08749de3dcc5"


@pytest.mark.parametrize("vector", FIXTURE["vectors"], ids=lambda vector: vector["name"])
@pytest.mark.parametrize(
    "overrides",
    [
        {"now": FIXTURE["created"] + 301},
        {"keyid": "product/v1"},
        {"target_uri": "https://attacker.example/topup/settlements"},
        {"body": b"{}"},
    ],
    ids=["stale", "wrong_keyid", "wrong_target", "altered_body"],
)
def test_rust_settlement_signature_rejects_mismatches(
    vector: dict[str, Any], overrides: dict[str, Any]
) -> None:
    with pytest.raises(SignatureError):
        _verify(vector, **overrides)
