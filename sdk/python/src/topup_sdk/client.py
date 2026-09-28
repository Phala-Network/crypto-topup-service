"""Retrying wrapper over the generated `topup_client` package, authenticated with a secret key.

Every request sends `Authorization: Bearer ppay_sk_…`; the key selects the account and the mode.
Every operation exposed here is idempotent on the service side, so the wrapper retries transport
failures, transient statuses, and `409 idempotency_key_in_use`:

- `create_quote`: sends an `Idempotency-Key` (generated unless given) and reuses it on every
  retry, so a retry returns the quote the first attempt created.
- `cancel_quote`: canceling a canceled quote returns it unchanged.
- `create_refund`: sends an `Idempotency-Key` like `create_quote`.

With a pinned `forwarder`, `create_quote` and `get_quote` recompute an open quote's address from
the factory, the implementation, the treasury, the account, and the quote id, and raise
`AddressMismatchError` rather than return an address the merchant did not derive.

`Quote.payment` reports a transfer seen before finality. It is display only: nothing is credited
until the deposit is final and appears under `list_deposits`, and a reorg can remove it.
"""

from __future__ import annotations

import time
import uuid
from collections.abc import Callable, Iterator
from functools import partial
from typing import Any, TypeVar

import httpx

from topup_client import AuthenticatedClient
from topup_client.api.account import get_account
from topup_client.api.attestation import get_attestation
from topup_client.api.config import get_config
from topup_client.api.deposits import get_deposit, list_deposits
from topup_client.api.quotes import cancel_quote, create_quote, get_quote
from topup_client.api.refunds import create_refund, get_refund
from topup_client.models import (
    AccountObject,
    AttestationResponse,
    Config,
    CreateQuoteRequest,
    CreateRefundRequest,
    Deposit,
    DepositList,
    ErrorResponse,
    Quote,
    Refund,
)
from topup_client.types import UNSET, Response, Unset

from .addresses import forwarder_address, lock_salt, same_address
from .attestation import verify_attestation_binding
from .errors import AddressMismatchError, ApiError
from .signing import sf_string

T = TypeVar("T")

RETRYABLE_STATUSES = frozenset({429, 500, 502, 503, 504})

SECRET_KEY_PREFIXES = ("ppay_sk_test_", "ppay_sk_live_")


class TopupClient:
    """Merchant API client authenticated with a secret key, `ppay_sk_test_…` or `ppay_sk_live_…`.

    `forwarder` is the `(factory, implementation, treasury)` triple pinned from the attested
    deployment and the merchant's treasury, as the settlement key is; given it, open quotes are
    checked before they are returned. The check needs the account id (`acct_…`): pass `account`,
    or the client reads it once from `GET /v1/account`.
    """

    def __init__(
        self,
        base_url: str,
        api_key: str,
        *,
        account: str | None = None,
        forwarder: tuple[str, str, str] | None = None,
        timeout: float = 15.0,
        max_attempts: int = 4,
        initial_backoff: float = 0.5,
        transport: httpx.BaseTransport | None = None,
        sleep: Callable[[float], None] = time.sleep,
    ) -> None:
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        if not api_key.startswith(SECRET_KEY_PREFIXES):
            raise ValueError("an API key is a secret key, ppay_sk_test_… or ppay_sk_live_…")
        self._account = account
        self.forwarder = forwarder
        self._max_attempts = max_attempts
        self._initial_backoff = initial_backoff
        self._sleep = sleep
        http = httpx.Client(
            base_url=base_url,
            headers={"Authorization": f"Bearer {api_key}"},
            timeout=timeout,
            follow_redirects=False,
            transport=transport,
        )
        self._client = AuthenticatedClient(
            base_url=base_url, token=api_key, raise_on_unexpected_status=False
        ).set_httpx_client(http)

    def close(self) -> None:
        """Closes the underlying connection pool."""
        self._client.get_httpx_client().close()

    def __enter__(self) -> TopupClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def get_account(self) -> AccountObject:
        """Returns the key's account, in the key's mode."""
        return self._call(lambda: get_account.sync_detailed(client=self._client), AccountObject)

    def account_id(self) -> str:
        """The key's account id, `acct_…`, read once."""
        if self._account is None:
            self._account = self.get_account().id
        return self._account

    def get_config(self) -> Config:
        """Returns the payable assets, limits, and quote terms the product's UI shows."""
        return self._call(lambda: get_config.sync_detailed(client=self._client), Config)

    def create_quote(
        self,
        account_id: str,
        amount: int,
        *,
        chain_id: int,
        asset: str,
        currency: str = "usd",
        idempotency_key: str | None = None,
    ) -> Quote:
        """Quotes `amount` minor units (cents) for `account_id`, payable in `asset` on `chain_id`.

        Retries reuse one `Idempotency-Key`, so they return the quote the first attempt created;
        pass your own key to make a retry after a crash safe too.
        """
        # The IETF Idempotency-Key header is an RFC 8941 string; the key is its content.
        key = sf_string(idempotency_key or str(uuid.uuid4()))
        body = CreateQuoteRequest(
            account_id=account_id,
            amount=amount,
            currency=currency,
            chain_id=chain_id,
            asset=asset,
        )
        quote = self._call(
            lambda: create_quote.sync_detailed(client=self._client, body=body, idempotency_key=key),
            Quote,
        )
        return self._checked(quote)

    def get_quote(self, quote_id: str) -> Quote:
        """Returns a quote, for example to resume a checkout page."""
        quote = self._call(lambda: get_quote.sync_detailed(quote_id, client=self._client), Quote)
        return self._checked(quote)

    def cancel_quote(self, quote_id: str) -> Quote:
        """Cancels an open, unpaid quote; later payments to its address are credited at spot."""
        return self._call(lambda: cancel_quote.sync_detailed(quote_id, client=self._client), Quote)

    def list_deposits(
        self,
        *,
        account_id: str | None = None,
        quote: str | None = None,
        status: str | None = None,
        tx_hash: str | None = None,
        created_gte: int | None = None,
        created_lte: int | None = None,
        expand: list[str] | None = None,
        page_size: int = 100,
    ) -> Iterator[Deposit]:
        """Yields the product's deposits matching the filters, newest first, following every page
        (Stripe's auto-pagination). `created_*` are Unix seconds; `expand` may name `data.quote`."""
        starting_after: str | None = None
        while True:
            page = self._call(
                partial(
                    list_deposits.sync_detailed,
                    client=self._client,
                    account_id=_unset(account_id),
                    quote=_unset(quote),
                    status=_unset(status),
                    tx_hash=_unset(tx_hash),
                    createdgte=_unset(created_gte),
                    createdlte=_unset(created_lte),
                    limit=page_size,
                    starting_after=_unset(starting_after),
                    expand=_unset(expand),
                ),
                DepositList,
            )
            yield from page.data
            if not page.has_more or not page.data:
                return
            starting_after = page.data[-1].id

    def get_deposit(self, deposit_id: str, *, expand: list[str] | None = None) -> Deposit:
        """Returns one of the product's deposits; `expand` may name `quote`."""
        return self._call(
            lambda: get_deposit.sync_detailed(
                deposit_id, client=self._client, expand=_unset(expand)
            ),
            Deposit,
        )

    def create_refund(
        self,
        deposit: str,
        destination_address: str,
        amount_atomic: int | None = None,
        *,
        idempotency_key: str | None = None,
    ) -> Refund:
        """Requests a refund of `deposit` (the unrefunded remainder unless `amount_atomic` is
        given) to an address the customer controls; finance approves and executes it.

        Retries reuse one `Idempotency-Key`, as `create_quote` does.
        """
        key = sf_string(idempotency_key or str(uuid.uuid4()))
        body = CreateRefundRequest(
            deposit=deposit,
            destination_address=destination_address,
            amount_atomic=UNSET if amount_atomic is None else str(amount_atomic),
        )
        return self._call(
            lambda: create_refund.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            Refund,
        )

    def get_refund(self, refund_id: str, *, expand: list[str] | None = None) -> Refund:
        """Returns one refund; `expand` may name `deposit`."""
        return self._call(
            lambda: get_refund.sync_detailed(refund_id, client=self._client, expand=_unset(expand)),
            Refund,
        )

    def attestation(self, nonce: bytes) -> AttestationResponse:
        """Fetches attestation evidence binding `nonce` to the settlement key.

        Raises `AttestationError` unless `report_data` binds `nonce` and `settlement_pubkey`.
        Verify the quote with the dstack verifier (`deploy/dstack-verifier.sh`), including that
        its report data is `report_data` zero-padded to 64 bytes, before pinning the key.
        """
        response = self._call(
            lambda: get_attestation.sync_detailed(client=self._client, nonce=nonce.hex()),
            AttestationResponse,
        )
        verify_attestation_binding(response, nonce)
        return response

    def _checked(self, quote: Quote) -> Quote:
        """Raises unless an open quote's address is the one derived from the pinned forwarder."""
        if self.forwarder is None or quote.status != "open":
            return quote
        factory, implementation, treasury = self.forwarder
        salt = lock_salt(self.account_id(), quote.account_id, quote.id)
        derived = forwarder_address(factory, implementation, treasury, salt)
        if not same_address(derived, quote.address):
            raise AddressMismatchError(f"quote {quote.id} has an address the account cannot derive")
        return quote

    def _call(self, operation: Callable[[], Response[Any]], expected: type[T]) -> T:
        attempt = 1
        while True:
            try:
                response = operation()
            except httpx.TransportError:
                if attempt >= self._max_attempts:
                    raise
            else:
                parsed = response.parsed
                if response.status_code == 200 and isinstance(parsed, expected):
                    return parsed
                error = _api_error(response)
                retryable = response.status_code in RETRYABLE_STATUSES or (
                    error.code == "idempotency_key_in_use"
                )
                if not retryable or attempt >= self._max_attempts:
                    raise error
            self._sleep(self._initial_backoff * 2 ** (attempt - 1))
            attempt += 1


def _unset[V](value: V | None) -> V | Unset:
    return UNSET if value is None else value


def _api_error(response: Response[Any]) -> ApiError:
    parsed = response.parsed
    if isinstance(parsed, ErrorResponse):
        error = parsed.error
        return ApiError(
            response.status_code,
            error.code,
            error.message,
            error_type=error.type_.value,
            param=error.param if isinstance(error.param, str) else None,
        )
    return ApiError(response.status_code, "unexpected_response", "undocumented response")
