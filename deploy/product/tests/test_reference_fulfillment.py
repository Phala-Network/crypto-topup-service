"""The reference product credits each `deposit.credited` once and holds what it refuses."""

from __future__ import annotations

import json
import sys
import time
import uuid
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from reference_product.config import DRIVER_KEYID, MissingProductKeyError, ProductConfig
from reference_product.fulfillment import Answer, Fulfillment, PinnedKeys
from reference_product.ledger import ProductLedger
from reference_product.server import AccountApi
from topup_sdk import RequestSigner, credited_event_id, load_public_key, sign_webhook
from topup_sdk.addresses import deposit_id

TEAM = "team-1"
SERVICE_KEY = Ed25519PrivateKey.from_private_bytes(bytes([3] * 32))

CONFIG = ProductConfig(
    service_url="http://service.test",
    product_slug="acct_" + "ac" * 16,
    api_key_file="unused",
    route="sandbox-acme-tpha-usd",
    chain_id=11155111,
    rpc_url="http://rpc.test",
    factory="0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d",
    implementation="0xfeb1871c9897251C74b39DFC74e577888290faE6",
    treasury="0x0000000000000000000000000000000000007EA5",
    token="0x" + "44" * 20,
    token_symbol="PHA",  # noqa: S106 - an asset symbol, not a secret
    listen_host="127.0.0.1",
    listen_port=0,
    public_url="https://acme.example/topup",
    payer="0x" + "55" * 20,
    per_deposit_cap_minor=10_000,
    per_period_cap_minor=15_000,
)


def _fulfillment(*, suspended: bool = False) -> Fulfillment:
    ledger = ProductLedger()
    ledger.add_team(TEAM, suspended=suspended)
    pinned = PinnedKeys(livemode=False, keys=[SERVICE_KEY.public_key()])
    return Fulfillment(CONFIG, ledger, lambda: pinned)


def _credited(
    number: int = 1,
    amount_minor: int = 2_500,
    team: str = TEAM,
    *,
    account: str = CONFIG.product_slug,
    livemode: bool = False,
) -> tuple[dict[str, str], bytes]:
    tx_hash = "0x" + f"{number:02x}" * 32
    deposit = deposit_id(CONFIG.chain_id, tx_hash, 0)
    event_id = credited_event_id(deposit)
    body = json.dumps(
        {
            "id": event_id,
            "object": "event",
            "account": account,
            "livemode": livemode,
            "type": "deposit.credited",
            "created": 1_790_410_321,
            "data": {
                "object": {
                    "id": deposit,
                    "object": "deposit",
                    "account_id": team,
                    "quote": "qt_" + "0c" * 16,
                    "status": "credited",
                    "rejection_reason": None,
                    "chain_id": CONFIG.chain_id,
                    "asset": "pha",
                    "asset_contract": CONFIG.token,
                    "amount_atomic": "25000000000000000000",
                    "amount": amount_minor,
                    "currency": "usd",
                    "exchange_rate": "0.10000000",
                    "price_source": "quote",
                    "valued_at": 1_790_410_320,
                    "address": "0x" + "66" * 20,
                    "from_address": "0x" + "77" * 20,
                    "tx_hash": tx_hash,
                    "log_index": 0,
                    "block_number": 100,
                    "amount_refunded_atomic": "0",
                    "refunded": False,
                    "created": 1_790_410_300,
                }
            },
        }
    ).encode()
    return sign_webhook(SERVICE_KEY, event_id, int(time.time()), body), body


def test_a_credit_is_applied_once_across_redeliveries() -> None:
    fulfillment = _fulfillment()
    headers, body = _credited()
    for _ in range(3):
        assert fulfillment.handle(headers, body).status == 204
    [(key, amount)] = fulfillment.ledger.credits_for(TEAM)
    assert amount == 2_500
    assert key.startswith("dep_")
    assert len(fulfillment.ledger.events("deposit.credited")) == 1


def test_a_forged_delivery_is_refused_without_a_credit() -> None:
    fulfillment = _fulfillment()
    headers, body = _credited()
    forged = sign_webhook(
        Ed25519PrivateKey.generate(), headers["webhook-id"], int(time.time()), body
    )
    assert fulfillment.handle(forged, body).status == 400
    assert fulfillment.handle(headers, body + b" ").status == 400
    assert fulfillment.ledger.credits_for(TEAM) == []


def test_a_repeat_with_another_amount_keeps_the_first_credit() -> None:
    fulfillment = _fulfillment()
    fulfillment.handle(*_credited(amount_minor=2_500))
    fulfillment.handle(*_credited(amount_minor=2_600))
    assert [amount for _, amount in fulfillment.ledger.credits_for(TEAM)] == [2_500]


@pytest.mark.parametrize(
    ("suspended", "amounts", "team", "reason"),
    [
        (True, [2_500], TEAM, "account_suspended"),
        (False, [10_001], TEAM, "per_deposit_cap"),
        (False, [10_000, 6_000], TEAM, "per_period_cap"),
    ],
)
def test_refused_credits_are_held_for_refund(
    suspended: bool, amounts: list[int], team: str, reason: str
) -> None:
    fulfillment = _fulfillment(suspended=suspended)
    for number, amount in enumerate(amounts, start=1):
        assert fulfillment.handle(*_credited(number, amount, team)).status == 204
    held = [order for order in fulfillment.ledger.orders_for(TEAM) if order["status"] == "held"]
    assert [order["reason"] for order in held] == [reason]
    credited = sum(amount for _, amount in fulfillment.ledger.credits_for(TEAM))
    assert credited == sum(amounts[:-1])


def test_a_credit_for_an_unknown_workspace_is_held() -> None:
    fulfillment = _fulfillment()
    assert fulfillment.handle(*_credited(team="team-unknown")).status == 204
    order = fulfillment.ledger.find_order(deposit_id(CONFIG.chain_id, "0x" + "01" * 32, 0))
    assert order is not None
    assert (order.status, order.reason, order.team_id) == ("held", "unknown_account", None)


def test_a_legacy_credited_event_is_refused_without_a_credit() -> None:
    # The envelope before `evt_` ids names no account, so it no longer verifies.
    fulfillment = _fulfillment()
    legacy_id = str(uuid.UUID(int=7))
    legacy = {
        "event_id": legacy_id,
        "type": "deposit.credited",
        "created_at": "2026-09-28T00:00:00Z",
        "data": {"deposit_id": str(uuid.UUID(int=8)), "external_id": TEAM, "state": "credited"},
    }
    legacy_body = json.dumps(legacy).encode()
    legacy_headers = sign_webhook(SERVICE_KEY, legacy_id, int(time.time()), legacy_body)
    assert fulfillment.handle(legacy_headers, legacy_body).status == 400
    assert fulfillment.ledger.credits_for(TEAM) == []


def test_another_accounts_or_modes_event_is_refused_without_a_credit() -> None:
    fulfillment = _fulfillment()
    for delivery in (
        _credited(account="acct_" + "0b" * 16),
        _credited(livemode=True),
    ):
        assert fulfillment.handle(*delivery).status == 400
    assert fulfillment.ledger.credits_for(TEAM) == []


def test_deliveries_wait_until_the_webhook_keys_are_pinned() -> None:
    def unpinned() -> PinnedKeys:
        raise MissingProductKeyError("not sealed yet")

    fulfillment = Fulfillment(CONFIG, ProductLedger(), unpinned)
    assert fulfillment.handle(*_credited()).status == 503


def test_orders_keyed_by_the_old_deposit_key_are_migrated(tmp_path: Path) -> None:
    deposit = uuid.UUID(int=9)
    path = str(tmp_path / "ledger.sqlite")
    before = ProductLedger(path)
    before.add_team(TEAM)
    with before.transaction() as db:
        db.execute(
            "INSERT INTO orders (id, team_id, provider, order_flow_code, provider_order_id, "
            "payload, status, created_at) VALUES ('o1', ?, 'crypto_topup', 'crypto-top-up', ?, "
            "'{}', 'accepted', 0)",
            (TEAM, f"deposit:{deposit}"),
        )
    # Opening the ledger applies the schema, which rewrites the old keys.
    assert ProductLedger(path).find_order(f"dep_{deposit.hex}") is not None


DRIVER = RequestSigner.from_seed(DRIVER_KEYID, bytes([7] * 32))


def _account_call(
    api: AccountApi, method: str, path: str, body: bytes, signer: RequestSigner = DRIVER
) -> Answer:
    target = "/topup" + path
    headers = signer.sign(method, "https://acme.example" + target, body)
    return api.handle(method, target, headers, body)


def test_account_api_requires_the_driver_key_and_valid_refs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("ACME_SEED", raising=False)
    config = replace(CONFIG, api_key_file=None, api_key_env="ACME_SEED")
    api = AccountApi(config, ProductLedger(), load_public_key(DRIVER.public_key_base64()))
    register = json.dumps({"account_id": TEAM}).encode()
    other = RequestSigner.from_seed(DRIVER_KEYID, bytes([8] * 32))
    assert _account_call(api, "POST", "/accounts", register, other).status == 401
    unsigned = api.handle("POST", "/topup/accounts", {}, register)
    assert unsigned.status == 401
    bad_ref = json.dumps({"account_id": "a/b"}).encode()
    assert _account_call(api, "POST", "/accounts", bad_ref).status == 400
    assert _account_call(api, "GET", f"/accounts/{TEAM}", b"").status == 404
    # Registration is the product's own; a quote needs the service, and the product key is not
    # sealed yet: unavailable.
    assert _account_call(api, "POST", "/accounts", register).status == 200
    quote = json.dumps({"amount_minor": 2500}).encode()
    assert _account_call(api, "POST", f"/accounts/{TEAM}/quotes", quote).status == 503


def test_refund_requests_only_name_the_workspaces_own_deposits() -> None:
    own, other = "dep_" + uuid.uuid4().hex, "dep_" + uuid.uuid4().hex
    requested: list[tuple[str, str, int]] = []

    class Service:
        def list_deposits(self, *, account_id: str) -> list[SimpleNamespace]:
            return [SimpleNamespace(id=own)] if account_id == TEAM else []

        def create_refund(self, deposit: str, to: str, amount: int) -> SimpleNamespace:
            requested.append((deposit, to, amount))
            return SimpleNamespace(to_dict=lambda: {"id": "re_1", "status": "pending"})

    ledger = ProductLedger()
    ledger.add_team(TEAM)
    api = AccountApi(CONFIG, ledger, load_public_key(DRIVER.public_key_base64()))
    api._client = Service()  # type: ignore[assignment]
    to = "0x" + "66" * 20

    def refund(deposit: str, body: dict[str, Any]) -> Answer:
        path = f"/accounts/{TEAM}/deposits/{deposit}/refunds"
        return _account_call(api, "POST", path, json.dumps(body).encode())

    body = {"destination_address": to, "amount_atomic": "5"}
    assert refund(other, body).status == 404
    assert refund("not-a-deposit-id", body).status == 400
    assert refund(own, {**body, "destination_address": "0x12"}).status == 400
    assert refund(own, {**body, "amount_atomic": "0"}).status == 400
    assert requested == []
    answer = refund(own, body)
    assert (answer.status, answer.body) == (200, {"id": "re_1", "status": "pending"})
    assert requested == [(own, to, 5)]
