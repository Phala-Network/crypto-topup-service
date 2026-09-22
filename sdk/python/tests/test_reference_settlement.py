"""The example's reference settlement endpoint stores only durable answers."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "examples"))

import phala_cloud_integration as reference
from topup_sdk import RequestSigner, load_public_key
from topup_sdk.addresses import deposit_id, forwarder_address, persistent_salt

FACTORY = "0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d"
IMPLEMENTATION = "0xfeb1871c9897251C74b39DFC74e577888290faE6"
TOKEN = "0x" + "44" * 20
TEAM = "team-1"
TX_HASH = "0x" + "ab" * 32
BLOCK_HASH = "0x" + "cd" * 32
AMOUNT = 25 * 10**18
SETTLEMENT_SEED = bytes([3] * 32)
PUBLIC_URL = "https://acme.example/topup"

CONFIG = reference.SandboxConfig(
    service_url="http://service.test",
    product_slug="acme",
    product_keyid="acme/v1",
    product_seed_file="unused",
    route="sandbox-acme-tpha-usd",
    chain_id=11155111,
    rpc_url="http://rpc.test",
    factory=FACTORY,
    implementation=IMPLEMENTATION,
    token=TOKEN,
    token_symbol="PHA",  # noqa: S106 - an asset symbol, not a secret
    listen_host="127.0.0.1",
    listen_port=0,
    public_url=PUBLIC_URL,
    payer="0x" + "55" * 20,
)
ADDRESS = forwarder_address(FACTORY, IMPLEMENTATION, persistent_salt("acme", TEAM, 1))
KEY = f"deposit:{deposit_id(CONFIG.chain_id, TX_HASH, 0)}"


def _log(**overrides: Any) -> dict[str, Any]:
    log = {
        "logIndex": "0x0",
        "address": TOKEN,
        "topics": [
            reference.TRANSFER_TOPIC,
            "0x" + "00" * 12 + "55" * 20,
            "0x" + "00" * 12 + ADDRESS[2:].lower(),
        ],
        "data": hex(AMOUNT),
    }
    log.update(overrides)
    return log


def _receipt(**overrides: Any) -> dict[str, Any]:
    receipt = {"status": "0x1", "blockNumber": "0x10", "blockHash": BLOCK_HASH, "logs": [_log()]}
    receipt.update(overrides)
    return receipt


class FakeRpc(reference.JsonRpc):
    def __init__(
        self,
        receipt: dict[str, Any] | None,
        *,
        finalized: int = 0x20,
        block_hash: str | None = BLOCK_HASH,
    ) -> None:
        super().__init__("http://rpc.test")
        self.receipt = receipt
        self.finalized = finalized
        self.block_hash = block_hash

    def call(self, method: str, params: list[Any]) -> Any:
        if method == "eth_getTransactionReceipt":
            return self.receipt
        if params[0] == "finalized":
            return {"number": hex(self.finalized)}
        return None if self.block_hash is None else {"hash": self.block_hash}


def _service(rpc: reference.JsonRpc) -> reference.SettlementService:
    ledger = reference.ProductLedger()
    ledger.add_team(TEAM)
    ledger.record_address(ADDRESS, TEAM, version=1)
    signer = RequestSigner.from_seed("settlement/v1", SETTLEMENT_SEED)
    return reference.SettlementService(
        CONFIG, ledger, load_public_key(signer.public_key_base64()), rpc
    )


def _post(service: reference.SettlementService) -> reference.Answer:
    payload = {
        "version": 1,
        "idempotency_key": KEY,
        "account_id": TEAM,
        "unit": "USD",
        "amount_minor": "115",
        "source": "crypto_deposit",
        "evidence": {
            "chain_id": CONFIG.chain_id,
            "asset_contract": TOKEN,
            "route": CONFIG.route,
            "route_version": 1,
            "tx_hash": TX_HASH,
            "log_index": 0,
            "to": ADDRESS,
            "amount_atomic": str(AMOUNT),
            "price_scaled": "4600000",
            "price_scale": 8,
            "valuation_at": "2026-09-22T00:00:00Z",
            "lock_ref": None,
        },
    }
    body = json.dumps(payload).encode()
    idempotency_key = f'"{KEY}"'
    signer = RequestSigner.from_seed("settlement/v1", SETTLEMENT_SEED)
    headers = signer.sign(
        "POST", PUBLIC_URL + "/settlements", body, idempotency_key=idempotency_key
    )
    headers["idempotency-key"] = idempotency_key
    return service.handle_post("/topup/settlements", headers, body)


@pytest.mark.parametrize(
    "rpc",
    [
        FakeRpc(None),
        FakeRpc(_receipt(), finalized=0x0F),
        FakeRpc(_receipt(), block_hash=None),
        FakeRpc(_receipt(), block_hash="0x" + "ee" * 32),
        FakeRpc({"status": "0x1", "logs": []}),
        FakeRpc(_receipt(logs=[{"logIndex": "0x0"}])),
    ],
    ids=[
        "receipt_missing",
        "not_finalized",
        "block_missing",
        "block_not_canonical",
        "receipt_malformed",
        "log_malformed",
    ],
)
def test_transient_chain_reads_answer_503_and_store_nothing(rpc: FakeRpc) -> None:
    service = _service(rpc)
    assert _post(service).status == 503
    assert service.ledger.find_order(KEY) is None
    assert service.ledger.credits_for(TEAM) == []


@pytest.mark.parametrize(
    ("receipt", "reason"),
    [
        (_receipt(status="0x0"), "transaction_reverted"),
        (_receipt(logs=[]), "log_not_found"),
        (_receipt(logs=[_log(address="0x" + "99" * 20)]), "log_not_emitted_by_asset"),
        (_receipt(logs=[_log(data=hex(AMOUNT + 1))]), "log_amount_mismatch"),
    ],
)
def test_finalized_mismatches_are_stored_refusals(receipt: dict[str, Any], reason: str) -> None:
    service = _service(FakeRpc(receipt))
    answer = _post(service)
    assert (answer.status, answer.body) == (200, {"status": "rejected", "reason": reason})
    order = service.ledger.find_order(KEY)
    assert order is not None
    assert order.status == "rejected"
    assert service.ledger.credits_for(TEAM) == []


def test_verified_deposit_is_credited_once() -> None:
    service = _service(FakeRpc(_receipt()))
    first = _post(service)
    assert first.status == 200
    assert first.body is not None
    assert first.body["status"] == "accepted"
    assert _post(service).body == first.body
    assert service.ledger.credits_for(TEAM) == [(KEY, 115)]
