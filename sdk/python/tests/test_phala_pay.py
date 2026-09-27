from __future__ import annotations

import base64
import json
import time
from pathlib import Path

import httpx
import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from phala_pay import Deposit, PhalaPay, Quote, SignatureVerificationError, Webhook
from topup_sdk import load_public_key, sign_webhook, verify_request
from topup_sdk.signing import target_uri

SEED = bytes([7] * 32)
SEED_PUBLIC_KEY = base64.b64encode(
    Ed25519PrivateKey.from_private_bytes(SEED)
    .public_key()
    .public_bytes(Encoding.Raw, PublicFormat.Raw)
).decode()
SERVICE_KEY = Ed25519PrivateKey.from_private_bytes(bytes([9] * 32))
SERVICE_PUBLIC_KEY = base64.b64encode(
    SERVICE_KEY.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
).decode()
QUOTE_ID = "qt_" + "0c" * 16
EVENT_ID = "evt_" + "26" * 16
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
    }


# Resources ---------------------------------------------------------------------------------------


def _client(handler: httpx.MockTransport) -> PhalaPay:
    return PhalaPay("http://service.test", "acme/v1", seed=SEED, transport=handler)


def test_quotes_create_returns_the_client_secret_from_a_signed_request() -> None:
    public_key = load_public_key(SEED_PUBLIC_KEY)
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        verify_request(
            method=request.method,
            target_uri=target_uri(request),
            headers=dict(request.headers),
            body=request.content,
            public_key=public_key,
            keyid="acme/v1",
            require_idempotency_key=True,
        )
        seen.append(request)
        return httpx.Response(200, json=_quote(client_secret=f"{QUOTE_ID}_secret_{'ab' * 24}"))

    with _client(httpx.MockTransport(handler)) as client:
        quote = client.quotes.create(
            account_id="team-42", amount=2500, chain_id=11155111, asset="pha", idempotency_key="o-1"
        )
    assert quote.client_secret == f"{QUOTE_ID}_secret_{'ab' * 24}"
    assert seen[0].headers["idempotency-key"] == '"o-1"'
    assert json.loads(seen[0].content)["account_id"] == "team-42"


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


@pytest.mark.parametrize("key", ["file", "hex"])
def test_the_key_loads_from_a_seed_file_or_hex(tmp_path: Path, key: str) -> None:
    seed_file = tmp_path / "product.seed"
    seed_file.write_text(SEED.hex() + "\n", encoding="ascii")
    signed: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        signed.append(request)
        return httpx.Response(200, json=_quote())

    transport = httpx.MockTransport(handler)
    client = (
        PhalaPay("http://service.test", "acme/v1", key_file=seed_file, transport=transport)
        if key == "file"
        else PhalaPay("http://service.test", "acme/v1", seed=SEED.hex(), transport=transport)
    )
    client.quotes.retrieve(QUOTE_ID)
    verify_request(
        method="GET",
        target_uri=target_uri(signed[0]),
        headers=dict(signed[0].headers),
        body=b"",
        public_key=load_public_key(SEED_PUBLIC_KEY),
        keyid="acme/v1",
        require_idempotency_key=False,
    )


def test_exactly_one_key_source_is_required() -> None:
    with pytest.raises(ValueError, match="exactly one"):
        PhalaPay("http://service.test", "acme/v1")
    with pytest.raises(ValueError, match="exactly one"):
        PhalaPay("http://service.test", "acme/v1", seed=SEED, key_file="product.seed")


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
    body = b'{"event_id": "x"}'
    headers = sign_webhook(SERVICE_KEY, EVENT_ID, int(time.time()), body)
    with pytest.raises(ValueError, match="not an event"):
        Webhook.construct_event(body, headers, SERVICE_PUBLIC_KEY)
