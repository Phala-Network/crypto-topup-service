"""A minimal product backend on FastAPI with Phala Pay: quotes for the checkout, and webhook
fulfillment.

- `POST /topups` `{"amount": 2500}` creates a quote for the signed-in team and returns its
  `client_secret`, which the browser passes to `<Checkout clientSecret apiBase />`.
- `POST /webhooks/phala-pay` verifies each delivery with `pay.webhooks.construct_event`, which
  fails closed unless it is signed by your account's key in the key's mode and names your account
  and mode, and, for `deposit.credited`, credits `amount` cents to `account_id` once per deposit
  id, in the same transaction that records the credit, before answering `200`.

Run it against staging (install with `uv add phala-pay fastapi uvicorn`):

    PHALA_PAY_API_BASE=https://pay.example.com \\
    PHALA_PAY_SECRET_KEY=ppay_sk_test_... \\
    PHALA_PAY_ACCOUNT=acct_... \\
    PHALA_PAY_WEBHOOK_KEYS=<your account's webhook public key in this mode, pinned from \\
        GET /v1/attestation; comma-separated while a rotation overlaps> \\
    uvicorn --factory fastapi_app:app_from_env
"""

from __future__ import annotations

import logging
import os
import sqlite3
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from typing import Annotated

import httpx
from fastapi import Depends, FastAPI, Header, HTTPException, Request
from pydantic import BaseModel, Field

from phala_pay import ApiError, PhalaPay, SignatureVerificationError

LOG = logging.getLogger(__name__)

SCHEMA = """
CREATE TABLE IF NOT EXISTS orders (
    id TEXT PRIMARY KEY, team TEXT NOT NULL, amount INTEGER NOT NULL, quote TEXT
);
CREATE TABLE IF NOT EXISTS credits (
    deposit TEXT PRIMARY KEY, team TEXT NOT NULL, amount INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS balances (team TEXT PRIMARY KEY, amount INTEGER NOT NULL);
"""


class TopupRequest(BaseModel):
    amount: int = Field(gt=0, le=10_000_000, description="US cents")


class TopupResponse(BaseModel):
    order_id: str
    client_secret: str


def current_team(x_team_id: Annotated[str, Header(pattern=r"^[A-Za-z0-9._-]{1,64}$")]) -> str:
    """Stands in for the product's session: replace it with your authentication."""
    return x_team_id


def create_app(
    pay: PhalaPay,
    webhook_keys: list[str],
    database: str,
    *,
    account: str,
    livemode: bool,
    chain_id: int,
    asset: str,
) -> FastAPI:
    app = FastAPI()

    @contextmanager
    def transaction() -> Iterator[sqlite3.Connection]:
        connection = sqlite3.connect(database, isolation_level="IMMEDIATE")
        try:
            with connection:
                yield connection
        finally:
            connection.close()

    with transaction() as db:
        db.executescript(SCHEMA)

    @app.post("/topups")
    def create_topup(
        body: TopupRequest, team: Annotated[str, Depends(current_team)]
    ) -> TopupResponse:
        order_id = str(uuid.uuid4())
        with transaction() as db:
            db.execute(
                "INSERT INTO orders (id, team, amount) VALUES (?, ?, ?)",
                (order_id, team, body.amount),
            )
        try:
            # The order id as the Idempotency-Key: repeating the call returns the same quote
            # with a fresh client secret, for example to resume the checkout after a reload.
            quote = pay.quotes.create(
                account_id=team,
                amount=body.amount,
                chain_id=chain_id,
                asset=asset,
                idempotency_key=order_id,
            )
        except ApiError as error:
            # Codes such as `amount_too_small` are stable and safe to show; messages are not.
            status = 400 if error.status_code in (400, 409, 429) else 502
            raise HTTPException(status, detail={"code": error.code}) from error
        except httpx.HTTPError as error:
            raise HTTPException(503, detail={"code": "unavailable"}) from error
        if not isinstance(quote.client_secret, str):
            raise HTTPException(502, detail={"code": "unexpected_response"})
        with transaction() as db:
            db.execute("UPDATE orders SET quote = ? WHERE id = ?", (quote.id, order_id))
        return TopupResponse(order_id=order_id, client_secret=quote.client_secret)

    @app.post("/webhooks/phala-pay")
    async def webhook(request: Request) -> dict[str, bool]:
        payload = await request.body()
        try:
            event = pay.webhooks.construct_event(
                payload, request.headers, webhook_keys, account, expected_livemode=livemode
            )
        except (SignatureVerificationError, ValueError) as error:
            raise HTTPException(400) from error

        if event.type == "deposit.credited":
            deposit = event.deposit
            if not isinstance(deposit.amount, int):
                # A credited deposit always has an amount; a 5xx makes the service retry.
                raise HTTPException(500)
            with transaction() as db:
                inserted = db.execute(
                    "INSERT OR IGNORE INTO credits (deposit, team, amount) VALUES (?, ?, ?)",
                    (deposit.id, deposit.account_id, deposit.amount),
                ).rowcount
                if inserted:
                    db.execute(
                        "INSERT INTO balances (team, amount) VALUES (?, ?) "
                        "ON CONFLICT (team) DO UPDATE SET amount = amount + excluded.amount",
                        (deposit.account_id, deposit.amount),
                    )
            LOG.info("deposit %s credited: %s", deposit.id, bool(inserted))
        return {"received": True}

    return app


def app_from_env() -> FastAPI:
    secret_key = os.environ["PHALA_PAY_SECRET_KEY"]
    pay = PhalaPay(os.environ["PHALA_PAY_API_BASE"], secret_key)
    return create_app(
        pay,
        [key.strip() for key in os.environ["PHALA_PAY_WEBHOOK_KEYS"].split(",") if key.strip()],
        os.environ.get("DATABASE", "topups.sqlite3"),
        account=os.environ["PHALA_PAY_ACCOUNT"],
        livemode=secret_key.startswith("ppay_sk_live_"),
        chain_id=int(os.environ.get("PHALA_PAY_CHAIN_ID", "11155111")),
        asset=os.environ.get("PHALA_PAY_ASSET", "pha"),
    )
