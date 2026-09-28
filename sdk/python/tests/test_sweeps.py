"""Offline sweeping: the factory's `flush` calldata and Safe Transaction Builder batch files."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from eth_abi.abi import encode
from eth_utils.crypto import keccak

from topup_sdk import (
    batch_checksum,
    flush_transaction,
    flush_transactions,
    safe_batch,
    write_safe_batch,
)
from topup_sdk.sweeps import _serialize

FACTORY = "0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d"
TREASURY = "0x0000000000000000000000000000000000007EA5"
SAFE = "0xDF8a1Ce35c9a6ACE153B4e0767942f1E2291a1Aa"
TOKEN = "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E"  # noqa: S105 - a token contract
SALTS = ["0x" + "01" * 32, "0x" + "02" * 32]
# The batch file both SDKs write for the same inputs; sdk/js checks it against `BatchFile`.
FIXTURE = Path(__file__).resolve().parents[2] / "testdata" / "safe-batch.json"


def test_flush_calldata_is_the_abi_encoding_of_the_factory_call() -> None:
    call = flush_transaction(FACTORY, TREASURY.lower(), SALTS, TOKEN.lower())
    selector = keccak(text="flush(address,bytes32[],address)")[:4]
    expected = selector + encode(
        ["address", "bytes32[]", "address"],
        [TREASURY, [bytes.fromhex(salt[2:]) for salt in SALTS], TOKEN],
    )
    assert call == {"to": FACTORY, "data": "0x" + expected.hex(), "value": "0"}
    with pytest.raises(ValueError, match="at least one salt"):
        flush_transaction(FACTORY, TREASURY, [], TOKEN)
    with pytest.raises(ValueError, match="32 bytes"):
        flush_transaction(FACTORY, TREASURY, ["0x01"], TOKEN)


def test_forwarders_group_into_one_call_per_factory_and_treasury() -> None:
    other = "0x" + "99" * 20

    def forwarder(salt: str, treasury: str = TREASURY) -> SimpleNamespace:
        return SimpleNamespace(chain_id=1, factory=FACTORY, treasury=treasury, salt=salt)

    forwarders = [forwarder(SALTS[0]), forwarder(SALTS[1], other), forwarder("0x" + "03" * 32)]
    calls = flush_transactions(forwarders, TOKEN, max_salts=1)
    assert calls == [
        flush_transaction(FACTORY, TREASURY, [SALTS[0]], TOKEN),
        flush_transaction(FACTORY, TREASURY, ["0x" + "03" * 32], TOKEN),
        flush_transaction(FACTORY, other, [SALTS[1]], TOKEN),
    ]
    other_chain = SimpleNamespace(**{**vars(forwarder(SALTS[1])), "chain_id": 10})
    with pytest.raises(ValueError, match="one chain"):
        flush_transactions([forwarder(SALTS[0]), other_chain], TOKEN)


def test_the_checksum_is_the_transaction_builders_own() -> None:
    # safe-react-apps apps/tx-builder/src/lib/checksum.test.js at
    # e8cccfb9a1042fa2954087988bae59c3b8c81780: `addChecksum(batchFileObject)`, whose meta holds
    # `checksum: ''` while it is computed, gives this value.
    address = "0x49d4450977E2c95362C13D3a31a09311E0Ea26A6"
    method = {"inputs": [], "name": "", "payable": False}
    batch = {
        "version": "1.0",
        "chainId": "4",
        "createdAt": 1646321521061,
        "meta": {
            "name": None,
            "txBuilderVersion": "1.4.0",
            "checksum": "",
            "createdFromSafeAddress": SAFE,
            "createdFromOwnerAddress": address,
        },
        "transactions": [
            {
                "to": address,
                "value": "0",
                "contractMethod": {
                    **method,
                    "inputs": [
                        {"internalType": "address", "name": "paramAddress", "type": "address"}
                    ],
                    "name": "testAddress",
                },
                "contractInputsValues": {"paramAddress": address},
            },
            {
                "to": address,
                "value": "0",
                "contractMethod": {
                    **method,
                    "inputs": [{"internalType": "bool", "name": "paramBool", "type": "bool"}],
                    "name": "testBool",
                },
                "contractInputsValues": {"paramAddress": "", "paramBool": "false"},
            },
            {
                "to": address,
                "value": "2000000000000000000",
                "data": "0x42f45790" + "00" * 12 + "49d4450977e2c95362c13d3a31a09311e0ea26a6",
            },
        ],
    }
    assert (
        "0x" + keccak(text=_serialize(batch)).hex()
        == "0x4ecbfd364aa6759983915644e73f8bd411e85a2dc306f252a387c2728c4db64c"
    )


def _batch() -> dict[str, Any]:
    calls = [
        flush_transaction(FACTORY, TREASURY, SALTS, TOKEN),
        flush_transaction(FACTORY, TREASURY, ["0x" + "03" * 32], TOKEN),
    ]
    return safe_batch(1, SAFE, calls, name="Phala Pay sweep", created_at_ms=1_790_000_000_000)


def test_a_safe_batch_is_a_transaction_builder_batch_file(tmp_path: Path) -> None:
    batch = _batch()
    # The fields and types of the app's `BatchFile` (apps/tx-builder/src/typings/models.ts).
    assert set(batch) == {"version", "chainId", "createdAt", "meta", "transactions"}
    assert batch["version"] == "1.0"
    assert batch["chainId"] == "1"
    assert isinstance(batch["createdAt"], int)
    assert set(batch["meta"]) <= {
        "txBuilderVersion",
        "checksum",
        "createdFromSafeAddress",
        "createdFromOwnerAddress",
        "name",
        "description",
    }
    assert batch["meta"]["name"] == "Phala Pay sweep"
    assert batch["meta"]["createdFromSafeAddress"] == SAFE
    for transaction in batch["transactions"]:
        # Every number is a string, as the app's import requires; no method ABI with raw data.
        assert set(transaction) == {"to", "value", "data"}
        assert all(isinstance(value, str) for value in transaction.values())
        assert transaction["to"] == FACTORY
    # The app's `validateChecksum` deletes the checksum and recomputes it over the rest.
    assert batch["meta"]["checksum"] == batch_checksum(batch)
    tampered = json.loads(json.dumps(batch))
    tampered["transactions"][0]["value"] = "1"
    assert batch_checksum(tampered) != batch["meta"]["checksum"]

    written = write_safe_batch(tmp_path / "sweep.json", batch)
    assert json.loads(written.read_text(encoding="utf-8")) == batch
    assert json.loads(FIXTURE.read_text(encoding="utf-8")) == batch, (
        "sdk/testdata/safe-batch.json is shared with sdk/js; regenerate it from _batch()"
    )


def test_a_batch_refuses_malformed_calls() -> None:
    for call in [{"to": FACTORY, "data": "0x", "value": 1}, {"to": FACTORY, "data": "zz"}]:
        with pytest.raises(ValueError, match=r"value|data"):
            safe_batch(1, SAFE, [call])  # type: ignore[list-item]
    with pytest.raises(ValueError, match="at least one call"):
        safe_batch(1, SAFE, [])
