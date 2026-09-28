from __future__ import annotations

import base64
import json
import time

import httpx
import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from phala_pay import (
    AddressMismatchError,
    Deposit,
    PhalaPay,
    Quote,
    SignatureVerificationError,
    Webhook,
)
from topup_client.models import DepositMetadata, QuoteMetadata
from topup_sdk import deposit_address, load_public_key, sign_webhook

API_KEY = "ppay_sk_test_" + "B" * 43 + "000000"
SERVICE_KEY = Ed25519PrivateKey.from_private_bytes(bytes([9] * 32))
SERVICE_PUBLIC_KEY = base64.b64encode(
    SERVICE_KEY.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
).decode()
QUOTE_ID = "qt_" + "0c" * 16
EVENT_ID = "evt_" + "26" * 16
REFUND_ID = "re_" + "0d" * 16
ADDRESS = "0x" + "11" * 20


def _quote(**fields: object) -> dict[str, object]:
    return {
        "id": QUOTE_ID,
        "object": "quote",
        "account_id": "team-42",
        "amount": 2500,
        "currency": "usd",
        "chain_id": 11155111,
        "asset": "pha",
        "amount_atomic": "100",
        "exchange_rate": "25.00000000",
        "address": ADDRESS,
        "payment_uri": f"ethereum:0x{'22' * 20}@11155111/transfer?address={ADDRESS}&uint256=100",
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
        "account_id": "team-42",
        "quote": QUOTE_ID,
        "status": "credited",
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
    return PhalaPay("http://service.test", API_KEY, transport=handler)


def test_quotes_create_returns_the_client_secret_to_a_request_with_the_key() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["authorization"] == f"Bearer {API_KEY}"
        assert request.headers["idempotency-key"]
        seen.append(request)
        return httpx.Response(200, json=_quote(client_secret=f"{QUOTE_ID}_secret_{'ab' * 24}"))

    with _client(httpx.MockTransport(handler)) as client:
        quote = client.quotes.create(
            account_id="team-42", amount=2500, chain_id=11155111, asset="pha", idempotency_key="o-1"
        )
    assert quote.client_secret == f"{QUOTE_ID}_secret_{'ab' * 24}"
    assert seen[0].headers["idempotency-key"] == '"o-1"'
    assert json.loads(seen[0].content)["account_id"] == "team-42"


def test_metadata_is_sent_on_create_and_merged_by_update() -> None:
    seen: list[httpx.Request] = []
    refund: dict[str, object] = {
        "id": REFUND_ID,
        "object": "refund",
        "deposit": f"dep_{1:032x}",
        "amount_atomic": "100",
        "destination_address": ADDRESS,
        "status": "pending",
        "tx_hash": None,
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
            account_id="team-42",
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
        deposits = list(client.deposits.list(account_id="team-42"))
    assert [d.log_index for d in deposits] == [3, 2, 1]


ACCOUNT = "acct_" + "0c" * 16
FACTORY = "0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d"
IMPLEMENTATION = "0xfeb1871c9897251C74b39DFC74e577888290faE6"
TREASURY = "0x0000000000000000000000000000000000007EA5"
DEPOSIT_ADDRESS_ID = "da_" + "0d" * 16


def _deposit_address(version: int = 1, **fields: object) -> dict[str, object]:
    address = deposit_address(
        FACTORY,
        IMPLEMENTATION,
        TREASURY,
        account=ACCOUNT,
        livemode=False,
        client_reference_id="team-42",
        chain_id=11155111,
        asset="pha",
        version=version,
    )
    return {
        "id": DEPOSIT_ADDRESS_ID,
        "object": "deposit_address",
        "livemode": False,
        "client_reference_id": "team-42",
        "chain_id": 11155111,
        "asset": "pha",
        "address": address.lower(),
        "payment_uri": f"ethereum:0x{'22' * 20}@11155111/transfer?address={address.lower()}",
        "treasury": TREASURY.lower(),
        "version": version,
        "salt": "0x" + "00" * 32,
        "status": "active",
        "created": 1_790_000_000,
        "retired_at": None,
        "metadata": {},
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
                    _deposit_address(1, status="retired", treasury="0x" + "99" * 20),
                ],
            },
        )

    with PhalaPay(
        "http://service.test",
        API_KEY,
        account=ACCOUNT,
        forwarder=(FACTORY, IMPLEMENTATION, TREASURY),
        transport=httpx.MockTransport(handler),
    ) as client:
        created = client.deposit_addresses.create(
            client_reference_id="team-42", chain_id=11155111, asset="pha", metadata={"team": "42"}
        )
        cleared = client.deposit_addresses.update(created.id, metadata="")
        rotated = client.deposit_addresses.rotate(created.id)
        listed = list(client.deposit_addresses.list(client_reference_id="team-42"))
    assert created.version == 1
    assert rotated.version == 2
    assert [address.status for address in listed] == ["active", "retired"]
    assert json.loads(seen[0].content) == {
        "client_reference_id": "team-42",
        "chain_id": 11155111,
        "asset": "pha",
        "metadata": {"team": "42"},
    }
    assert created.metadata.to_dict() == {"team": "42"}
    assert json.loads(seen[1].content) == {"metadata": ""}
    assert cleared.metadata.to_dict() == {}
    assert seen[3].url.params["client_reference_id"] == "team-42"


def test_a_deposit_address_the_account_cannot_derive_is_refused() -> None:
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=_deposit_address(address="0x" + "11" * 20))

    with (
        PhalaPay(
            "http://service.test",
            API_KEY,
            account=ACCOUNT,
            forwarder=(FACTORY, IMPLEMENTATION, TREASURY),
            transport=httpx.MockTransport(handler),
        ) as client,
        pytest.raises(AddressMismatchError, match=DEPOSIT_ADDRESS_ID),
    ):
        client.deposit_addresses.retrieve(DEPOSIT_ADDRESS_ID)


def test_the_key_must_be_a_secret_key() -> None:
    with pytest.raises(ValueError, match="secret key"):
        PhalaPay("http://service.test", "acme/v1")


# Webhooks ----------------------------------------------------------------------------------------


def _delivery(
    event_type: str = "deposit.credited",
    obj: dict[str, object] | None = None,
    *,
    event_id: str = EVENT_ID,
    webhook_id: str = EVENT_ID,
    timestamp: int | None = None,
    key: Ed25519PrivateKey = SERVICE_KEY,
) -> tuple[bytes, dict[str, str]]:
    body = json.dumps(
        {
            "id": event_id,
            "object": "event",
            "type": event_type,
            "created": 1_790_000_321,
            "data": {"object": _deposit() if obj is None else obj},
        }
    ).encode()
    stamp = int(time.time()) if timestamp is None else timestamp
    return body, sign_webhook(key, webhook_id, stamp, body)


def test_construct_event_returns_the_typed_deposit() -> None:
    body, headers = _delivery()
    event = Webhook.construct_event(body, headers, SERVICE_PUBLIC_KEY)
    assert (event.id, event.type, event.created) == (EVENT_ID, "deposit.credited", 1_790_000_321)
    assert isinstance(event.data.object, Deposit)
    assert (event.deposit.id, event.deposit.account_id, event.deposit.amount) == (
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
    )
    assert isinstance(event.data.object, Quote)
    assert event.quote.status == "expired"


def test_construct_event_keeps_unknown_types_raw() -> None:
    body, headers = _delivery("payout.paid", {"id": "po_1"})
    assert Webhook.construct_event(body, headers, SERVICE_PUBLIC_KEY).data.object == {"id": "po_1"}


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
        Webhook.construct_event(body, headers, SERVICE_PUBLIC_KEY)


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
        Webhook.construct_event(body, headers, SERVICE_PUBLIC_KEY)
