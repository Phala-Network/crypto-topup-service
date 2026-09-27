from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from topup_sdk.addresses import (
    deposit_id,
    forwarder_address,
    lock_salt,
    same_address,
    to_checksum_address,
)

# Shared with contracts/ and crates/core/tests/create2_vectors.rs.
VECTORS: dict[str, Any] = json.loads(
    (Path(__file__).resolve().parents[3] / "contracts/test-vectors/create2.json").read_text(
        encoding="utf-8"
    )
)


@pytest.mark.parametrize("vector", VECTORS["lock"], ids=lambda vector: vector["lock_ref"])
def test_lock_salt_and_address_match_the_contract_vectors(vector: dict[str, Any]) -> None:
    salt = lock_salt(vector["product_slug"], vector["external_id"], vector["lock_ref"])
    assert "0x" + salt.hex() == vector["salt"]
    address = forwarder_address(VECTORS["factory"], VECTORS["implementation"], salt)
    assert address == vector["predicted_address"]


def test_raw_salts_match_the_contract_vectors() -> None:
    for salt, expected in zip(VECTORS["salts"], VECTORS["predictedAddresses"], strict=True):
        address = forwarder_address(
            VECTORS["factory"], VECTORS["implementation"], bytes.fromhex(salt[2:])
        )
        assert address == expected


def test_deposit_id_matches_the_core_vector() -> None:
    # crates/core/src/identity.rs::matches_python_uuid5_vector
    tx_hash = "0x0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"
    assert deposit_id(1, tx_hash, 42) == "dep_20513a599b8053df983208749de3dcc5"
    with pytest.raises(ValueError, match="32 bytes"):
        deposit_id(1, "0x01", 42)


def test_checksum_and_comparison() -> None:
    lower = VECTORS["factory"].lower()
    assert to_checksum_address(lower) == VECTORS["factory"]
    assert same_address(lower, VECTORS["factory"])
    assert not same_address(lower, VECTORS["implementation"])
