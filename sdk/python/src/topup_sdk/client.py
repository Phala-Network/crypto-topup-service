"""Signed, retrying wrapper over the generated `topup_client` package.

The product is the signer's key id, `{product}/v1`. Every operation exposed here is idempotent on
the service side, so the wrapper retries transport failures, transient statuses, and
`409 signature_replayed` with a freshly signed request each time:

- `create_quote`: sends an `Idempotency-Key` (generated unless given) and reuses it on every
  retry, so a retry returns the quote the first attempt created.
- `cancel_quote`: canceling a canceled quote returns it unchanged.
- `create_deposit_address` / `get_deposit_address`: return the current persistent address.
- `rotate_deposit_address`: idempotent on `from_version`; a replay returns the same new version.
- `request_refund`: idempotent on `(deposit, to_address, amount)`.

With a pinned `forwarder`, `create_quote` and `get_quote` recompute an open quote's address from
the factory, the implementation, and the quote id, and raise `AddressMismatchError` rather than
return an address the product did not derive.

`list_pending_deposits` and `Quote.payment` report transfers seen before finality. They are
display only: nothing is credited until the deposit is final and appears under `list_deposits`,
and a reorg can remove a pending transfer.
"""

from __future__ import annotations

import time
import uuid
from collections.abc import Callable, Iterator
from datetime import datetime
from functools import partial
from typing import Any, TypeVar

import httpx

from topup_client import AuthenticatedClient
from topup_client.api.addresses import (
    create_deposit_address,
    get_deposit_address,
    rotate_deposit_address,
)
from topup_client.api.attestation import get_attestation
from topup_client.api.config import get_config
from topup_client.api.deposits import (
    get_deposit,
    list_deposits,
    list_pending_deposits,
    lookup_deposits,
)
from topup_client.api.quotes import cancel_quote, create_quote, get_quote
from topup_client.api.refunds import request_refund
from topup_client.models import (
    AttestationResponse,
    Config,
    CreateQuoteRequest,
    DepositAddressResponse,
    DepositResponse,
    DepositsResponse,
    ErrorResponse,
    PendingDepositResponse,
    PendingDepositsResponse,
    Quote,
    RefundRequest,
    RefundResponse,
    RotateDepositAddressRequest,
    SupportDepositResponse,
    SupportDepositsResponse,
)
from topup_client.types import UNSET, Response

from .addresses import forwarder_address, lock_salt, same_address
from .attestation import verify_attestation_binding
from .errors import AddressMismatchError, ApiError
from .signing import RequestSigner, SigningAuth, sf_string

T = TypeVar("T")

RETRYABLE_STATUSES = frozenset({429, 500, 502, 503, 504})


PRODUCT_KEYID_SUFFIX = "/v1"


class TopupClient:
    """Product API client that signs every request with the product key.

    `forwarder` is the `(factory, implementation)` pair pinned from the attested deployment, as
    the settlement key is; given it, open quotes are checked before they are returned.
    """

    def __init__(
        self,
        base_url: str,
        signer: RequestSigner,
        *,
        forwarder: tuple[str, str] | None = None,
        timeout: float = 15.0,
        max_attempts: int = 4,
        initial_backoff: float = 0.5,
        transport: httpx.BaseTransport | None = None,
        sleep: Callable[[float], None] = time.sleep,
    ) -> None:
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        if not signer.keyid.endswith(PRODUCT_KEYID_SUFFIX):
            raise ValueError("a product key id is `{product}/v1`")
        self.product_slug = signer.keyid.removesuffix(PRODUCT_KEYID_SUFFIX)
        self.forwarder = forwarder
        self._max_attempts = max_attempts
        self._initial_backoff = initial_backoff
        self._sleep = sleep
        http = httpx.Client(
            base_url=base_url,
            auth=SigningAuth(signer),
            timeout=timeout,
            follow_redirects=False,
            transport=transport,
        )
        # The API authenticates with RFC 9421 signatures from `SigningAuth`, not a bearer token;
        # installing our own httpx client keeps the generated code from adding one.
        self._client = AuthenticatedClient(
            base_url=base_url, token="", raise_on_unexpected_status=False
        ).set_httpx_client(http)

    def close(self) -> None:
        """Closes the underlying connection pool."""
        self._client.get_httpx_client().close()

    def __enter__(self) -> TopupClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

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

    def create_deposit_address(self, external_id: str) -> DepositAddressResponse:
        """Returns the account's persistent address, creating version 1 on first use."""
        return self._call(
            lambda: create_deposit_address.sync_detailed(
                self.product_slug, external_id, client=self._client
            ),
            DepositAddressResponse,
        )

    def get_deposit_address(self, external_id: str) -> DepositAddressResponse:
        """Returns the account's current persistent address."""
        return self._call(
            lambda: get_deposit_address.sync_detailed(
                self.product_slug, external_id, client=self._client
            ),
            DepositAddressResponse,
        )

    def rotate_deposit_address(self, external_id: str, from_version: int) -> DepositAddressResponse:
        """Rotates from `from_version` to the next version; older addresses stay valid."""
        return self._call(
            lambda: rotate_deposit_address.sync_detailed(
                self.product_slug,
                external_id,
                client=self._client,
                body=RotateDepositAddressRequest(from_version=from_version),
            ),
            DepositAddressResponse,
        )

    def list_deposits(
        self,
        external_id: str,
        *,
        state: str | None = None,
        created_from: datetime | None = None,
        created_to: datetime | None = None,
    ) -> Iterator[DepositResponse]:
        """Yields the account's deposits, newest first, following every page."""
        cursor: uuid.UUID | None = None
        while True:
            page = self._call(
                partial(
                    list_deposits.sync_detailed,
                    self.product_slug,
                    external_id,
                    client=self._client,
                    state=UNSET if state is None else state,
                    from_=UNSET if created_from is None else created_from,
                    to=UNSET if created_to is None else created_to,
                    cursor=UNSET if cursor is None else cursor,
                ),
                DepositsResponse,
            )
            yield from page.deposits
            if not isinstance(page.next_cursor, uuid.UUID):
                return
            cursor = page.next_cursor

    def list_pending_deposits(self, external_id: str) -> list[PendingDepositResponse]:
        """Returns transfers to the account's persistent addresses seen before finality.

        These are not deposits and have not been credited; show them as "received, waiting for
        finality" and credit only from `list_deposits` or `deposit.credited`.
        """
        return self._call(
            lambda: list_pending_deposits.sync_detailed(
                self.product_slug, external_id, client=self._client
            ),
            PendingDepositsResponse,
        ).pending_deposits

    def get_deposit(self, deposit_id: uuid.UUID) -> DepositResponse:
        """Returns one deposit owned by this product."""
        return self._call(
            lambda: get_deposit.sync_detailed(self.product_slug, deposit_id, client=self._client),
            DepositResponse,
        )

    def lookup_deposits(
        self,
        *,
        tx_hash: str | None = None,
        address: str | None = None,
        lock_ref: str | None = None,
    ) -> Iterator[SupportDepositResponse]:
        """Yields support-lookup matches with their full transition timelines."""
        cursor: str | None = None
        while True:
            page = self._call(
                partial(
                    lookup_deposits.sync_detailed,
                    self.product_slug,
                    client=self._client,
                    tx_hash=UNSET if tx_hash is None else tx_hash,
                    address=UNSET if address is None else address,
                    lock_ref=UNSET if lock_ref is None else lock_ref,
                    cursor=UNSET if cursor is None else cursor,
                ),
                SupportDepositsResponse,
            )
            yield from page.deposits
            if not isinstance(page.next_cursor, str):
                return
            cursor = page.next_cursor

    def request_refund(
        self, deposit_id: uuid.UUID, to_address: str, amount_atomic: int
    ) -> RefundResponse:
        """Files a refund request for finance review; replays return the same request."""
        return self._call(
            lambda: request_refund.sync_detailed(
                self.product_slug,
                deposit_id,
                client=self._client,
                body=RefundRequest(to_address=to_address, amount=str(amount_atomic)),
            ),
            RefundResponse,
        )

    def attestation(self, nonce: bytes) -> AttestationResponse:
        """Fetches attestation evidence binding `nonce` to the settlement key and operators.

        Raises `AttestationError` unless `report_data` binds `nonce`, `settlement_pubkey`, and
        every entry of `operators`. Verify the quote with the dstack verifier
        (`deploy/dstack-verifier.sh`), including that its report data is `report_data` zero-padded
        to 64 bytes, before pinning the key or trusting an operator.
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
        factory, implementation = self.forwarder
        salt = lock_salt(self.product_slug, quote.account_id, quote.id)
        if not same_address(forwarder_address(factory, implementation, salt), quote.address):
            raise AddressMismatchError(f"quote {quote.id} has an address the product cannot derive")
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
                    error.code == "signature_replayed"
                )
                if not retryable or attempt >= self._max_attempts:
                    raise error
            self._sleep(self._initial_backoff * 2 ** (attempt - 1))
            attempt += 1


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
