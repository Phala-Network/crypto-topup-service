from __future__ import annotations

import base64
import json
import time
from typing import Any

import httpx
import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from phala_pay import (
    AddressMismatchError,
    Deposit,
    Event,
    EventRequest,
    PhalaPay,
    Quote,
    Refund,
    SignatureVerificationError,
    Webhook,
)
from topup_client.models import DepositMetadata, QuoteMetadata
from topup_sdk import deposit_address, load_public_key, quote_address, sign_webhook

API_KEY = "ppay_sk_test_" + "B" * 43 + "000000"
SERVICE_KEY = Ed25519PrivateKey.from_private_bytes(bytes([9] * 32))
SERVICE_PUBLIC_KEY = base64.b64encode(
    SERVICE_KEY.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
).decode()
QUOTE_ID = "qt_" + "0c" * 16
EVENT_ID = "evt_" + "26" * 16
REFUND_ID = "re_" + "0d" * 16
ADDRESS = "0x" + "11" * 20
ACCOUNT = "acct_" + "a1" * 16


def _quote(**fields: object) -> dict[str, object]:
    address = quote_address(
        FACTORY,
        IMPLEMENTATION,
        TREASURY,
        account=ACCOUNT,
        client_reference_id="team-42",
        quote_id=QUOTE_ID,
    )
    return {
        "id": QUOTE_ID,
        "object": "quote",
        "livemode": False,
        "client_reference_id": "team-42",
        "treasury": TREASURY.lower(),
        "metadata": {},
        "amount": 2500,
        "currency": "usd",
        "chain_id": 11155111,
        "asset": "pha",
        "amount_atomic": "100",
        "exchange_rate": "25.00000000",
        "address": address,
        "payment_uri": f"ethereum:0x{'22' * 20}@11155111/transfer?address={address}&uint256=100",
        "status": "open",
        "expires_at": 1_790_000_900,
        "created": 1_790_000_000,
        "payment": None,
        "deposit": None,
        **fields,
    }


def _deposit(index: int = 1) -> dict[str, object]:
    return {
        "id": f"dep_{index:032x}",
        "object": "deposit",
        "livemode": False,
        "client_reference_id": "team-42",
        "quote": QUOTE_ID,
        "deposit_address": None,
        "status": "credited",
        "final": False,
        "swept": False,
        "rejection_reason": None,
        "chain_id": 11155111,
        "asset": "pha",
        "asset_contract": "0x" + "22" * 20,
        "amount_atomic": "100",
        "amount": 2500,
        "currency": "usd",
        "exchange_rate": "25.00000000",
        "price_source": "quote",
        "valued_at": 1_790_000_300,
        "address": ADDRESS,
        "from_address": "0x" + "33" * 20,
        "tx_hash": "0x" + "ab" * 32,
        "log_index": index,
        "block_number": 1,
        "amount_refunded_atomic": "0",
        "refunded": False,
        "created": 1_790_000_300,
        "metadata": {"order_id": "6735"},
    }


# Resources ---------------------------------------------------------------------------------------


def _client(handler: httpx.MockTransport) -> PhalaPay:
    return PhalaPay(
        "http://service.test",
        API_KEY,
        account=ACCOUNT,
        forwarder=(FACTORY, IMPLEMENTATION),
        transport=handler,
    )


def test_quotes_create_returns_the_client_secret_to_a_request_with_the_key() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["authorization"] == f"Bearer {API_KEY}"
        assert request.headers["idempotency-key"]
        seen.append(request)
        return httpx.Response(200, json=_quote(client_secret=f"{QUOTE_ID}_secret_{'ab' * 24}"))

    with _client(httpx.MockTransport(handler)) as client:
        quote = client.quotes.create(
            client_reference_id="team-42",
            amount=2500,
            chain_id=11155111,
            asset="pha",
            idempotency_key="o-1",
        )
    assert quote.client_secret == f"{QUOTE_ID}_secret_{'ab' * 24}"
    assert seen[0].headers["idempotency-key"] == '"o-1"'
    assert json.loads(seen[0].content)["client_reference_id"] == "team-42"


def test_metadata_is_sent_on_create_and_merged_by_update() -> None:
    seen: list[httpx.Request] = []
    refund: dict[str, object] = {
        "id": REFUND_ID,
        "object": "refund",
        "livemode": False,
        "deposit": f"dep_{1:032x}",
        "amount_atomic": "100",
        "destination_address": ADDRESS,
        "treasury": "0x" + "7e" * 20,
        "status": "pending",
        "failure_reason": None,
        "transaction_hash": None,
        "log_index": None,
        "created": 1_790_000_400,
        "metadata": {},
    }

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        body = json.loads(request.content) if request.content else {}
        metadata = body.get("metadata")
        merged = metadata if isinstance(metadata, dict) else {}
        if request.url.path.startswith("/v1/quotes"):
            return httpx.Response(200, json=_quote(metadata=merged))
        if request.url.path.startswith("/v1/deposits"):
            return httpx.Response(200, json={**_deposit(), "metadata": merged})
        return httpx.Response(200, json={**refund, "metadata": merged})

    with _client(httpx.MockTransport(handler)) as client:
        quote = client.quotes.create(
            client_reference_id="team-42",
            amount=2500,
            chain_id=11155111,
            asset="pha",
            metadata={"order_id": "6735"},
        )
        assert isinstance(quote.metadata, QuoteMetadata)
        assert quote.metadata.to_dict() == {"order_id": "6735"}
        client.quotes.update(QUOTE_ID, metadata={"order_id": "", "cart": "9"})
        deposit = client.deposits.update(f"dep_{1:032x}", metadata="")
        assert isinstance(deposit.metadata, DepositMetadata)
        assert deposit.metadata.to_dict() == {}
        client.refunds.create(
            deposit=f"dep_{1:032x}", destination_address=ADDRESS, metadata={"ticket": "T-1"}
        )
        client.refunds.update(REFUND_ID, metadata={"ticket": "T-2"})
        client.quotes.update(QUOTE_ID)
        with pytest.raises(ValueError, match="unset every key"):
            client.quotes.update(QUOTE_ID, metadata="x")  # type: ignore[arg-type]
    sent = [(r.method, r.url.path, json.loads(r.content)) for r in seen]
    assert sent == [
        ("POST", "/v1/quotes", {**json.loads(seen[0].content), "metadata": {"order_id": "6735"}}),
        ("POST", f"/v1/quotes/{QUOTE_ID}", {"metadata": {"order_id": "", "cart": "9"}}),
        ("POST", f"/v1/deposits/dep_{1:032x}", {"metadata": ""}),
        ("POST", "/v1/refunds", {**json.loads(seen[3].content), "metadata": {"ticket": "T-1"}}),
        ("POST", f"/v1/refunds/{REFUND_ID}", {"metadata": {"ticket": "T-2"}}),
        ("POST", f"/v1/quotes/{QUOTE_ID}", {}),
    ]


def test_deposits_list_follows_every_page() -> None:
    pages = {None: [_deposit(3), _deposit(2)], "dep_" + f"{2:032x}": [_deposit(1)]}

    def handler(request: httpx.Request) -> httpx.Response:
        after = request.url.params.get("starting_after")
        data = pages[after]
        return httpx.Response(
            200,
            json={"object": "list", "url": "/v1/deposits", "has_more": after is None, "data": data},
        )

    with _client(httpx.MockTransport(handler)) as client:
        deposits = list(client.deposits.list(client_reference_id="team-42"))
    assert [d.log_index for d in deposits] == [3, 2, 1]


ACCOUNT = "acct_" + "0c" * 16
FACTORY = "0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d"
IMPLEMENTATION = "0xfeb1871c9897251C74b39DFC74e577888290faE6"
TREASURY = "0x0000000000000000000000000000000000007EA5"
DEPOSIT_ADDRESS_ID = "da_" + "0d" * 16


def _network(chain_id: int, address: str, treasury: str = TREASURY) -> dict[str, object]:
    return {
        "chain_id": chain_id,
        "address": address,
        "treasury": treasury.lower(),
        "assets": [
            {
                "asset": "pha",
                "contract": "0x" + "22" * 20,
                "decimals": 18,
                "payment_uri": f"ethereum:0x{'22' * 20}@{chain_id}/transfer?address={address}",
            }
        ],
    }


def _deposit_address(version: int = 1, **fields: object) -> dict[str, object]:
    address = deposit_address(
        FACTORY,
        IMPLEMENTATION,
        TREASURY,
        account=ACCOUNT,
        livemode=False,
        client_reference_id="team-42",
        version=version,
    ).lower()
    return {
        "id": DEPOSIT_ADDRESS_ID,
        "object": "deposit_address",
        "livemode": False,
        "client_reference_id": "team-42",
        "address": address,
        "version": version,
        "salt": "0x" + "00" * 32,
        "status": "active",
        "created": 1_790_000_000,
        "retired_at": None,
        "metadata": {},
        "networks": [_network(11155111, address), _network(84532, address)],
        "payments": [],
        **fields,
    }


def test_deposit_addresses_create_rotate_and_list_check_every_active_address() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        if request.url.path == "/v1/deposit_addresses" and request.method == "POST":
            return httpx.Response(200, json=_deposit_address(metadata={"team": "42"}))
        if request.url.path == f"/v1/deposit_addresses/{DEPOSIT_ADDRESS_ID}":
            return httpx.Response(200, json=_deposit_address(metadata={}))
        if request.url.path.endswith("/rotate"):
            assert request.headers["idempotency-key"]
            return httpx.Response(200, json=_deposit_address(2))
        return httpx.Response(
            200,
            json={
                "object": "list",
                "url": "/v1/deposit_addresses",
                "has_more": False,
                "data": [
                    _deposit_address(2),
                    _deposit_address(
                        1,
                        status="retired",
                        address=None,
                        networks=[_network(11155111, "0x" + "99" * 20, "0x" + "99" * 20)],
                    ),
                ],
            },
        )

    with PhalaPay(
        "http://service.test",
        API_KEY,
        account=ACCOUNT,
        forwarder=(FACTORY, IMPLEMENTATION),
        transport=httpx.MockTransport(handler),
    ) as client:
        created = client.deposit_addresses.create(
            client_reference_id="team-42", metadata={"team": "42"}
        )
        cleared = client.deposit_addresses.update(created.id, metadata="")
        rotated = client.deposit_addresses.rotate(created.id)
        listed = list(client.deposit_addresses.list(client_reference_id="team-42"))
    assert created.version == 1
    assert [network.chain_id for network in created.networks] == [11155111, 84532]
    assert created.networks[0].assets[0].asset == "pha"
    assert rotated.version == 2
    assert [address.status for address in listed] == ["active", "retired"]
    assert json.loads(seen[0].content) == {
        "client_reference_id": "team-42",
        "metadata": {"team": "42"},
    }
    assert created.metadata.to_dict() == {"team": "42"}
    assert json.loads(seen[1].content) == {"metadata": ""}
    assert cleared.metadata.to_dict() == {}
    assert seen[3].url.params["client_reference_id"] == "team-42"


OTHER_TREASURY = "0x" + "99" * 20


def _derived(treasury: str) -> str:
    return deposit_address(
        FACTORY,
        IMPLEMENTATION,
        treasury,
        account=ACCOUNT,
        livemode=False,
        client_reference_id="team-42",
        version=1,
    )


@pytest.mark.parametrize(
    ("network", "treasuries"),
    [
        # Another address on one chain.
        (_network(84532, "0x" + "11" * 20), None),
        # The address of another treasury, which a pin of the account's treasuries refuses.
        (
            _network(84532, _derived(OTHER_TREASURY), OTHER_TREASURY),
            {11155111: TREASURY, 84532: TREASURY},
        ),
        # A chain without a pinned treasury.
        (_network(84532, _derived(TREASURY)), {11155111: TREASURY}),
    ],
)
def test_a_deposit_address_the_account_cannot_derive_is_refused(
    network: dict[str, object], treasuries: dict[int, str] | None
) -> None:
    body = _deposit_address()
    networks = body["networks"]
    assert isinstance(networks, list)

    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, json={**body, "address": None, "networks": [networks[0], network]}
        )

    with (
        PhalaPay(
            "http://service.test",
            API_KEY,
            account=ACCOUNT,
            forwarder=(FACTORY, IMPLEMENTATION),
            treasuries=treasuries,
            transport=httpx.MockTransport(handler),
        ) as client,
        pytest.raises(AddressMismatchError, match="chain 84532"),
    ):
        client.deposit_addresses.retrieve(DEPOSIT_ADDRESS_ID)


def test_the_key_must_be_a_secret_key() -> None:
    with pytest.raises(ValueError, match="secret key"):
        PhalaPay("http://service.test", "acme/v1", forwarder=(FACTORY, IMPLEMENTATION))


# Webhooks ----------------------------------------------------------------------------------------


def _delivery(
    event_type: str = "deposit.credited",
    obj: dict[str, object] | None = None,
    *,
    event_id: str = EVENT_ID,
    webhook_id: str = EVENT_ID,
    timestamp: int | None = None,
    key: Ed25519PrivateKey | list[Ed25519PrivateKey] = SERVICE_KEY,
    account: str = ACCOUNT,
    livemode: bool = False,
    extra: dict[str, object] | None = None,
) -> tuple[bytes, dict[str, str]]:
    data: dict[str, object] = {"object": _deposit() if obj is None else obj}
    body = json.dumps(
        {
            "id": event_id,
            "object": "event",
            "account": account,
            "livemode": livemode,
            "type": event_type,
            "created": 1_790_000_321,
            "request": None,
            "data": data,
            **(extra or {}),
        }
    ).encode()
    stamp = int(time.time()) if timestamp is None else timestamp
    return body, sign_webhook(key, webhook_id, stamp, body)


def _construct(
    body: bytes,
    headers: dict[str, str],
    key: str | list[str] = SERVICE_PUBLIC_KEY,
    account: str = ACCOUNT,
    livemode: bool = False,
) -> Event:
    return Webhook.construct_event(body, headers, key, account, expected_livemode=livemode)


def test_construct_event_returns_the_typed_deposit() -> None:
    body, headers = _delivery()
    event = _construct(body, headers)
    assert (event.id, event.type, event.created) == (EVENT_ID, "deposit.credited", 1_790_000_321)
    assert (event.account, event.livemode) == (ACCOUNT, False)
    assert isinstance(event.data.object, Deposit)
    assert (event.deposit.id, event.deposit.client_reference_id, event.deposit.amount) == (
        f"dep_{1:032x}",
        "team-42",
        2500,
    )
    # The quote's metadata, copied to its deposit, arrives with the event.
    assert isinstance(event.deposit.metadata, DepositMetadata)
    assert event.deposit.metadata.to_dict() == {"order_id": "6735"}
    with pytest.raises(TypeError):
        _ = event.quote


def test_construct_event_parses_quote_events_and_accepts_text_and_any_header_case() -> None:
    body, headers = _delivery("quote.expired", _quote(status="expired"))
    event = Webhook.construct_event(
        body.decode(),
        {k.upper(): v for k, v in headers.items()},
        load_public_key(SERVICE_PUBLIC_KEY),
        ACCOUNT,
        expected_livemode=False,
    )
    assert isinstance(event.data.object, Quote)
    assert event.quote.status == "expired"


def test_construct_event_parses_a_failed_refund() -> None:
    refund: dict[str, object] = {
        "id": "re_" + "0e" * 16,
        "object": "refund",
        "livemode": False,
        "deposit": f"dep_{1:032x}",
        "amount_atomic": "100",
        "destination_address": "0x" + "44" * 20,
        "treasury": "0x" + "7e" * 20,
        "status": "failed",
        "failure_reason": "sender_mismatch",
        "transaction_hash": "0x" + "dd" * 32,
        "log_index": None,
        "created": 1_790_000_000,
        "metadata": {},
    }
    body, headers = _delivery("refund.failed", refund)
    event = _construct(body, headers)
    assert isinstance(event.data.object, Refund)
    assert event.refund.status == "failed"
    assert event.refund.failure_reason == "sender_mismatch"
    with pytest.raises(TypeError):
        _ = event.deposit


def test_construct_event_carries_the_request_and_previous_attributes() -> None:
    body, headers = _delivery()
    assert _construct(body, headers).request is None
    endpoint = {"id": "we_" + "0a" * 16, "object": "webhook_endpoint", "url": "https://b.example"}
    body, headers = _delivery(
        "webhook_endpoint.updated",
        extra={
            "request": {"id": "req_" + "0b" * 16, "idempotency_key": "update-1"},
            "data": {"object": endpoint, "previous_attributes": {"url": "https://a.example"}},
        },
    )
    event = _construct(body, headers)
    assert event.request == EventRequest("req_" + "0b" * 16, "update-1")
    assert event.data.object == endpoint
    assert event.data.previous_attributes == {"url": "https://a.example"}


def test_construct_event_keeps_unknown_types_raw() -> None:
    body, headers = _delivery("payout.paid", {"id": "po_1"})
    assert _construct(body, headers).data.object == {"id": "po_1"}


@pytest.mark.parametrize(
    ("case", "match"),
    [
        ("tampered", "no valid webhook signature"),
        ("other key", "no valid webhook signature"),
        ("stale", "outside tolerance"),
        ("id mismatch", "does not match"),
        ("no headers", "headers missing"),
    ],
)
def test_construct_event_rejects_forgeries(case: str, match: str) -> None:
    if case == "tampered":
        body, headers = _delivery()
        body = body.replace(b"2500", b"9999")
    elif case == "other key":
        body, headers = _delivery(key=Ed25519PrivateKey.generate())
    elif case == "stale":
        body, headers = _delivery(timestamp=int(time.time()) - 301)
    elif case == "id mismatch":
        body, headers = _delivery(webhook_id="evt_" + "00" * 16)
    else:
        body, headers = _delivery()
        headers = {}
    with pytest.raises(SignatureVerificationError, match=match):
        _construct(body, headers)


def test_construct_event_rejects_a_verified_body_that_is_not_an_event() -> None:
    # The service's envelope before `evt_` ids, as an operator replay still sends it.
    legacy_id = "0b6f1e1e-6f0c-4c43-9d7a-2f0d4b0f7a11"
    body = json.dumps(
        {
            "event_id": legacy_id,
            "type": "deposit.credited",
            "created_at": "2026-09-26T00:00:00Z",
            "data": {"deposit_id": legacy_id},
        }
    ).encode()
    headers = sign_webhook(SERVICE_KEY, legacy_id, int(time.time()), body)
    with pytest.raises(ValueError, match="not an event"):
        _construct(body, headers)


@pytest.mark.parametrize(
    ("delivery", "account", "livemode", "match"),
    [
        ({"account": "acct_" + "b2" * 16}, ACCOUNT, False, "another account"),
        ({}, "acct_" + "b2" * 16, False, "another account"),
        ({"livemode": True}, ACCOUNT, False, "other mode"),
        ({}, ACCOUNT, True, "other mode"),
    ],
)
def test_construct_event_fails_closed_for_another_account_or_mode(
    delivery: dict[str, Any], account: str, livemode: bool, match: str
) -> None:
    body, headers = _delivery(**delivery)
    with pytest.raises(SignatureVerificationError, match=match):
        _construct(body, headers, account=account, livemode=livemode)


def test_construct_event_requires_the_expected_account() -> None:
    body, headers = _delivery()
    with pytest.raises(ValueError, match="expected_account"):
        _construct(body, headers, account="")
    with pytest.raises(TypeError):
        Webhook.construct_event(body, headers, SERVICE_PUBLIC_KEY)  # type: ignore[call-arg]


def test_construct_event_accepts_either_pinned_key_during_a_rotation() -> None:
    new_key = Ed25519PrivateKey.from_private_bytes(bytes([10] * 32))
    new_public = base64.b64encode(
        new_key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    ).decode()
    body, headers = _delivery(key=[new_key, SERVICE_KEY])
    for pinned in (SERVICE_PUBLIC_KEY, new_public, [new_public, SERVICE_PUBLIC_KEY]):
        assert _construct(body, headers, pinned).id == EVENT_ID
    # After the overlap only the new key signs: the old pin alone no longer verifies.
    body, headers = _delivery(key=new_key)
    with pytest.raises(SignatureVerificationError, match="no valid webhook signature"):
        _construct(body, headers)
    assert _construct(body, headers, [new_public, SERVICE_PUBLIC_KEY]).id == EVENT_ID
