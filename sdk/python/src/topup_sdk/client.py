"""Signed, retrying wrapper over the generated `topup_client` package.

Every operation exposed here is idempotent on the service side, so the wrapper retries transport
failures, transient statuses, and `409 signature_replayed` with a freshly signed request each
time:

- `register_account`: find-or-create by `(product, external_id)`.
- `create_deposit_address` / `get_deposit_address`: return the current persistent address.
- `rotate_deposit_address`: idempotent on `from_version`; a replay returns the same new version.
- `create_rate_lock`: idempotent on `lock_ref`; a replay with a different amount is a `409`.
- `cancel_rate_lock`: cancelling a cancelled lock returns the same result.
- `request_refund`: idempotent on `(deposit, to_address, amount)`.

`list_pending_deposits` and `RateLockResponse.payment` report transfers seen before finality.
They are display only: nothing is credited until the deposit is final and appears under
`list_deposits`, and a reorg can remove a pending transfer.
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
from topup_client.api.accounts import get_limits, register_account
from topup_client.api.addresses import (
    create_deposit_address,
    get_deposit_address,
    rotate_deposit_address,
)
from topup_client.api.attestation import get_attestation
from topup_client.api.deposits import (
    get_deposit,
    list_deposits,
    list_pending_deposits,
    lookup_deposits,
)
from topup_client.api.rate_locks import cancel_rate_lock, create_rate_lock, get_rate_lock
from topup_client.api.refunds import request_refund
from topup_client.models import (
    AccountResponse,
    AttestationResponse,
    CancelRateLockResponse,
    CreateRateLockRequest,
    DepositAddressResponse,
    DepositResponse,
    DepositsResponse,
    ErrorResponse,
    LimitsResponse,
    PendingDepositResponse,
    PendingDepositsResponse,
    RateLockResponse,
    RefundRequest,
    RefundResponse,
    RegisterAccountRequest,
    RotateDepositAddressRequest,
    SupportDepositResponse,
    SupportDepositsResponse,
)
from topup_client.types import UNSET, Response

from .attestation import verify_attestation_binding
from .errors import ApiError
from .signing import RequestSigner, SigningAuth

T = TypeVar("T")

RETRYABLE_STATUSES = frozenset({429, 500, 502, 503, 504})


class TopupClient:
    """Product API client that signs every request with the product key."""

    def __init__(
        self,
        base_url: str,
        product_slug: str,
        signer: RequestSigner,
        *,
        timeout: float = 15.0,
        max_attempts: int = 4,
        initial_backoff: float = 0.5,
        transport: httpx.BaseTransport | None = None,
        sleep: Callable[[float], None] = time.sleep,
    ) -> None:
        if max_attempts < 1:
            raise ValueError("max_attempts must be positive")
        self.product_slug = product_slug
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

    def register_account(self, external_id: str) -> AccountResponse:
        """Registers the account, or returns it unchanged when it already exists."""
        return self._call(
            lambda: register_account.sync_detailed(
                self.product_slug,
                client=self._client,
                body=RegisterAccountRequest(external_id=external_id),
            ),
            AccountResponse,
        )

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

    def create_rate_lock(
        self,
        external_id: str,
        lock_ref: str,
        *,
        amount_minor: int | None = None,
        amount_atomic: int | None = None,
    ) -> RateLockResponse:
        """Creates, or replays, the quote-first lock identified by `lock_ref`."""
        if (amount_minor is None) == (amount_atomic is None):
            raise ValueError("pass exactly one of amount_minor or amount_atomic")
        body = CreateRateLockRequest(
            product_lock_ref=lock_ref,
            amount_minor=UNSET if amount_minor is None else str(amount_minor),
            amount_atomic=UNSET if amount_atomic is None else str(amount_atomic),
        )
        return self._call(
            lambda: create_rate_lock.sync_detailed(
                self.product_slug, external_id, client=self._client, body=body
            ),
            RateLockResponse,
        )

    def get_rate_lock(self, external_id: str, lock_ref: str) -> RateLockResponse:
        """Returns a lock by reference, for example to resume a checkout page."""
        return self._call(
            lambda: get_rate_lock.sync_detailed(
                self.product_slug, external_id, lock_ref, client=self._client
            ),
            RateLockResponse,
        )

    def cancel_rate_lock(self, external_id: str, lock_ref: str) -> CancelRateLockResponse:
        """Cancels an unpaid lock; later payments to its address are credited at spot."""
        return self._call(
            lambda: cancel_rate_lock.sync_detailed(
                self.product_slug, external_id, lock_ref, client=self._client
            ),
            CancelRateLockResponse,
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

    def get_limits(self, external_id: str) -> LimitsResponse:
        """Returns route caps and the account's remaining open-lock exposure."""
        return self._call(
            lambda: get_limits.sync_detailed(self.product_slug, external_id, client=self._client),
            LimitsResponse,
        )

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
        return ApiError(response.status_code, parsed.error.code, parsed.error.message)
    return ApiError(response.status_code, "unexpected_response", "undocumented response")
