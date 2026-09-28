"""Retrying wrapper over the generated `topup_client` package, authenticated with a secret key.

Every request sends `Authorization: Bearer ppay_sk_…`; the key selects the account and the mode.
Every operation exposed here is idempotent on the service side, so the wrapper retries transport
failures, transient statuses, and `409 idempotency_key_in_use`:

- `create_quote`: sends an `Idempotency-Key` (generated unless given) and reuses it on every
  retry, so a retry returns the quote the first attempt created.
- `cancel_quote`: canceling a canceled quote returns it unchanged.
- `create_refund`: sends an `Idempotency-Key` like `create_quote`.
- `create_deposit_address`: returns the customer's active address, so a repeat returns the same
  one; `rotate_deposit_address` sends an `Idempotency-Key` like `create_quote`.
- `mark_refund_paid`: attaching the same transaction again returns the refund.
- `cancel_refund`: canceling a canceled refund returns it unchanged.
- `update_quote`, `update_deposit`, `update_refund`: merging the same `metadata` again leaves the
  object as the first attempt did.
- Every other `POST` (keys, webhook endpoints, treasuries, the account) sends one
  `Idempotency-Key` for all its attempts, so a retry returns the first attempt's response.

`metadata` is Stripe's: up to 50 string pairs, keys of up to 40 characters without square
brackets, values of up to 500 characters. On an update a key set to `""` is unset and
`metadata=""` unsets every key. A quote's metadata is copied to the deposit that pays it. Do not
store sensitive information in it.

With a pinned `forwarder`, the `(factory, implementation)` pair of the attested deployment,
every quote and deposit address is recomputed before it is returned: an open quote's address from
the quote's `treasury`, the account, the customer, and the quote id; every network of an active
deposit address from its `treasury` and salt inputs. A mismatch raises `AddressMismatchError`
rather than return an address the merchant did not derive. `treasuries`, the merchant's own
treasury per chain, additionally pins the treasury each address pays: an address over any other
treasury, or on a chain without a pinned one, is refused.

`Quote.payment` reports a transfer seen before finality. It is display only: nothing is credited
until the deposit is final and appears under `list_deposits`, and a reorg can remove it.
"""

from __future__ import annotations

import time
import uuid
from collections.abc import Callable, Iterator, Mapping, Sequence
from functools import partial
from typing import Any, Literal, TypeVar

import httpx

from topup_client import AuthenticatedClient
from topup_client.api.account import (
    get_account,
    pause_account,
    resume_account,
    roll_webhook_key,
    update_account,
)
from topup_client.api.api_keys import (
    create_api_key,
    get_api_key,
    list_api_keys,
    revoke_api_key,
    roll_api_key,
)
from topup_client.api.attestation import get_attestation
from topup_client.api.balance import get_balance
from topup_client.api.config import get_config
from topup_client.api.deposit_addresses import (
    create_deposit_address,
    get_deposit_address,
    list_deposit_addresses,
    rotate_deposit_address,
    update_deposit_address,
)
from topup_client.api.deposits import get_deposit, list_deposits, update_deposit
from topup_client.api.events import get_event, list_events, resend_event
from topup_client.api.forwarders import list_forwarders
from topup_client.api.quotes import (
    cancel_quote,
    create_quote,
    get_quote,
    list_quotes,
    update_quote,
)
from topup_client.api.refunds import (
    cancel_refund,
    create_refund,
    get_refund,
    list_refunds,
    mark_refund_paid,
    update_refund,
)
from topup_client.api.sweeps import list_sweeps
from topup_client.api.treasuries import (
    cancel_treasury,
    create_treasury,
    create_treasury_challenge,
    get_treasury,
    list_treasuries,
)
from topup_client.api.webhook_endpoints import (
    create_webhook_endpoint,
    delete_webhook_endpoint,
    get_webhook_endpoint,
    list_webhook_endpoints,
    test_webhook_endpoint,
    update_webhook_endpoint,
)
from topup_client.models import (
    AccountObject,
    AccountSelfPauseRequest,
    ApiKeyList,
    ApiKeyObject,
    AttestationResponse,
    Balance,
    Config,
    ConfirmationPolicy,
    CreateApiKeyRequest,
    CreateDepositAddressRequest,
    CreateQuoteRequest,
    CreateRefundRequest,
    CreateTreasuryChallengeRequest,
    CreateTreasuryRequest,
    CreateWebhookEndpointRequest,
    DeletedWebhookEndpoint,
    Deposit,
    DepositAddress,
    DepositAddressList,
    DepositList,
    ErrorResponse,
    EventList,
    EventObjectResponse,
    Forwarder,
    ForwarderList,
    MarkRefundPaidRequest,
    MetadataClear,
    MetadataParamType0,
    Quote,
    QuoteList,
    Refund,
    RefundList,
    ResendEventRequest,
    RollApiKeyRequest,
    RollWebhookKeyRequest,
    Sweep,
    SweepList,
    Treasury,
    TreasuryChallenge,
    TreasuryList,
    UpdateAccountObjectRequest,
    UpdateMetadataRequest,
    UpdateWebhookEndpointRequest,
    WebhookEndpointList,
    WebhookEndpointObject,
)
from topup_client.types import UNSET, Response, Unset

from .addresses import deposit_address, quote_address, same_address
from .attestation import verify_attestation_binding
from .errors import AddressMismatchError, ApiError
from .signing import sf_string

T = TypeVar("T")

# A `POST` retried with its `Idempotency-Key` after a `500` gets the saved `500` back, marked
# `Idempotent-Replayed`, which is not retried again.
RETRYABLE_STATUSES = frozenset({429, 500, 502, 503, 504})

SECRET_KEY_PREFIXES = ("ppay_sk_test_", "ppay_sk_live_")

Metadata = Mapping[str, str] | Literal[""]
"""A `metadata` parameter: string pairs, where `""` unsets a key, or `""` to unset every key."""


class TopupClient:
    """Merchant API client authenticated with a secret key, `ppay_sk_test_…` or `ppay_sk_live_…`.

    `forwarder` is the `(factory, implementation)` pair pinned from the attested deployment, as
    the webhook keys are; given it, quotes and deposit addresses are recomputed before they are
    returned, and `treasuries` (`{chain_id: treasury}`) pins the treasury each may pay. The check
    needs the account id (`acct_…`): pass `account`, or the client reads it once from
    `GET /v1/account`.
    """

    def __init__(
        self,
        base_url: str,
        api_key: str,
        *,
        account: str | None = None,
        forwarder: tuple[str, str] | None = None,
        treasuries: Mapping[int, str] | None = None,
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
        if treasuries is not None and forwarder is None:
            raise ValueError("pinning treasuries needs the forwarder to recompute addresses")
        self._account = account
        self.forwarder = forwarder
        self.treasuries = None if treasuries is None else dict(treasuries)
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

    def roll_webhook_key(self, *, expires_in: int = 0) -> AccountObject:
        """Rolls this mode's webhook signing key: the next version signs from now on, and the
        current one keeps signing beside it for `expires_in` seconds (at most 7 days; `0` stops it
        at once). Pin the new public key from `attestation` before the old one expires. Retries
        reuse one `Idempotency-Key`, so they never roll twice."""
        key = _idempotency_key(None)
        body = RollWebhookKeyRequest(expires_in=expires_in)
        return self._call(
            lambda: roll_webhook_key.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            AccountObject,
        )

    def update_account(
        self, *, confirmation_policies: Mapping[int, str | None] | None = None
    ) -> AccountObject:
        """Updates the account's settings in the key's mode. `confirmation_policies` maps a chain
        id to the confirmation its payments must reach before they are credited: a depth such as
        `"12"`, `"safe"`, or `"finalized"`, never weaker than the route's (`get_config`); `None`
        removes a chain's policy. Chains not listed keep theirs."""
        body = UpdateAccountObjectRequest(
            confirmation_policies=UNSET
            if confirmation_policies is None
            else [
                ConfirmationPolicy(chain_id=chain_id, confirmations=value)
                for chain_id, value in confirmation_policies.items()
            ]
        )
        key = _idempotency_key(None)
        return self._call(
            lambda: update_account.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            AccountObject,
        )

    def pause_quotes(self) -> AccountObject:
        """Pauses the account's `quotes` in both modes: no quote, deposit address, or network is
        issued until `resume_quotes`, for an emergency such as a leaked key during a treasury
        time-lock. Payments to existing addresses keep being credited."""
        key = _idempotency_key(None)
        body = AccountSelfPauseRequest(scopes=["quotes"])
        return self._call(
            lambda: pause_account.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            AccountObject,
        )

    def resume_quotes(self) -> AccountObject:
        """Lifts your own `quotes` pause; an operator's pause stays in `paused_scopes`."""
        key = _idempotency_key(None)
        body = AccountSelfPauseRequest(scopes=["quotes"])
        return self._call(
            lambda: resume_account.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            AccountObject,
        )

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
        client_reference_id: str,
        amount: int,
        *,
        chain_id: int,
        asset: str,
        currency: str = "usd",
        idempotency_key: str | None = None,
        metadata: Mapping[str, str] | None = None,
    ) -> Quote:
        """Quotes `amount` minor units (cents) for the customer `client_reference_id`, payable in
        `asset` on `chain_id`.

        Retries reuse one `Idempotency-Key`, so they return the quote the first attempt created;
        pass your own key to make a retry after a crash safe too. The deposit that pays the quote
        starts with a copy of its `metadata`.
        """
        key = _idempotency_key(idempotency_key)
        body = CreateQuoteRequest(
            client_reference_id=client_reference_id,
            amount=amount,
            currency=currency,
            chain_id=chain_id,
            asset=asset,
            metadata=_metadata(metadata),
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

    def list_quotes(
        self,
        *,
        client_reference_id: str | None = None,
        status: str | None = None,
        page_size: int = 100,
    ) -> Iterator[Quote]:
        """Yields the account's quotes, newest first, following every page. They are not
        recomputed: an expired or completed quote's address is no longer shown to anyone."""
        return self._paginate(
            lambda starting_after: partial(
                list_quotes.sync_detailed,
                client=self._client,
                client_reference_id=_unset(client_reference_id),
                status=_unset(status),
                limit=page_size,
                starting_after=_unset(starting_after),
            ),
            QuoteList,
        )

    def update_quote(self, quote_id: str, *, metadata: Metadata | None = None) -> Quote:
        """Merges `metadata` into the quote's, in any status."""
        body = UpdateMetadataRequest(metadata=_metadata(metadata))
        quote = self._call(
            lambda: update_quote.sync_detailed(quote_id, client=self._client, body=body), Quote
        )
        return self._checked(quote)

    def cancel_quote(self, quote_id: str) -> Quote:
        """Cancels an open, unpaid quote; later payments to its address are credited at spot."""
        return self._call(lambda: cancel_quote.sync_detailed(quote_id, client=self._client), Quote)

    def list_deposits(
        self,
        *,
        client_reference_id: str | None = None,
        quote: str | None = None,
        deposit_address: str | None = None,
        status: str | None = None,
        tx_hash: str | None = None,
        created_gt: int | None = None,
        created_gte: int | None = None,
        created_lt: int | None = None,
        created_lte: int | None = None,
        expand: list[str] | None = None,
        page_size: int = 100,
    ) -> Iterator[Deposit]:
        """Yields the account's deposits matching the filters, newest first, following every page
        (Stripe's auto-pagination). `created_*` are Unix seconds; `expand` may name `data.quote`.
        A deposit is `final` once it can no longer be reversed."""
        starting_after: str | None = None
        while True:
            page = self._call(
                partial(
                    list_deposits.sync_detailed,
                    client=self._client,
                    client_reference_id=_unset(client_reference_id),
                    quote=_unset(quote),
                    deposit_address=_unset(deposit_address),
                    status=_unset(status),
                    tx_hash=_unset(tx_hash),
                    createdgt=_unset(created_gt),
                    createdgte=_unset(created_gte),
                    createdlt=_unset(created_lt),
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

    def update_deposit(self, deposit_id: str, *, metadata: Metadata | None = None) -> Deposit:
        """Merges `metadata` into the deposit's; the quote's is left unchanged."""
        body = UpdateMetadataRequest(metadata=_metadata(metadata))
        return self._call(
            lambda: update_deposit.sync_detailed(deposit_id, client=self._client, body=body),
            Deposit,
        )

    def create_deposit_address(
        self,
        client_reference_id: str,
        *,
        metadata: Metadata | None = None,
    ) -> DepositAddress:
        """Returns the customer's active deposit address, one address for every supported token on
        every supported network (`networks`), issuing it the first time. Any amount of a supported
        token sent to it is credited at spot when it arrives. `metadata` is merged into the
        address's, and each deposit to it starts with a copy."""
        body = CreateDepositAddressRequest(
            client_reference_id=client_reference_id,
            metadata=_metadata(metadata),
        )
        address = self._call(
            lambda: create_deposit_address.sync_detailed(client=self._client, body=body),
            DepositAddress,
        )
        return self._checked_deposit_address(address)

    def get_deposit_address(self, deposit_address_id: str) -> DepositAddress:
        """Returns one deposit address, active or retired."""
        address = self._call(
            lambda: get_deposit_address.sync_detailed(deposit_address_id, client=self._client),
            DepositAddress,
        )
        return self._checked_deposit_address(address)

    def list_deposit_addresses(
        self,
        *,
        client_reference_id: str | None = None,
        status: str | None = None,
        page_size: int = 100,
    ) -> Iterator[DepositAddress]:
        """Yields the matching deposit addresses, newest first, following every page."""
        starting_after: str | None = None
        while True:
            page = self._call(
                partial(
                    list_deposit_addresses.sync_detailed,
                    client=self._client,
                    client_reference_id=_unset(client_reference_id),
                    status=_unset(status),
                    limit=page_size,
                    starting_after=_unset(starting_after),
                ),
                DepositAddressList,
            )
            for address in page.data:
                yield self._checked_deposit_address(address)
            if not page.has_more or not page.data:
                return
            starting_after = page.data[-1].id

    def update_deposit_address(
        self, deposit_address_id: str, *, metadata: Metadata | None = None
    ) -> DepositAddress:
        """Merges `metadata` into the deposit address's, active or retired; deposits already
        recorded keep their own copy."""
        body = UpdateMetadataRequest(metadata=_metadata(metadata))
        address = self._call(
            lambda: update_deposit_address.sync_detailed(
                deposit_address_id, client=self._client, body=body
            ),
            DepositAddress,
        )
        return self._checked_deposit_address(address)

    def rotate_deposit_address(
        self, deposit_address_id: str, *, idempotency_key: str | None = None
    ) -> DepositAddress:
        """Retires an active deposit address and returns the customer's new one. Payments to the
        retired address are still credited; stop showing it.

        Retries reuse one `Idempotency-Key`, so a retry never rotates twice.
        """
        key = _idempotency_key(idempotency_key)
        address = self._call(
            lambda: rotate_deposit_address.sync_detailed(
                deposit_address_id, client=self._client, idempotency_key=key
            ),
            DepositAddress,
        )
        return self._checked_deposit_address(address)

    def create_refund(
        self,
        deposit: str,
        destination_address: str,
        amount_atomic: int | None = None,
        *,
        idempotency_key: str | None = None,
        metadata: Mapping[str, str] | None = None,
    ) -> Refund:
        """Creates a pending refund of `deposit` (the unrefunded remainder unless `amount_atomic`
        is given) to an address the customer controls. Pay it from the refund's `treasury`, then
        attach the transaction with `mark_refund_paid`.

        Retries reuse one `Idempotency-Key`, as `create_quote` does.
        """
        key = _idempotency_key(idempotency_key)
        body = CreateRefundRequest(
            deposit=deposit,
            destination_address=destination_address,
            amount_atomic=UNSET if amount_atomic is None else str(amount_atomic),
            metadata=_metadata(metadata),
        )
        return self._call(
            lambda: create_refund.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            Refund,
        )

    def mark_refund_paid(
        self, refund_id: str, transaction_hash: str, *, log_index: int | None = None
    ) -> Refund:
        """Attaches the transaction that pays a pending refund; the service verifies it at
        finality. `log_index` names the paying `Transfer` log when one transaction pays several
        refunds."""
        body = MarkRefundPaidRequest(
            transaction_hash=transaction_hash,
            log_index=UNSET if log_index is None else log_index,
        )
        return self._call(
            lambda: mark_refund_paid.sync_detailed(refund_id, client=self._client, body=body),
            Refund,
        )

    def cancel_refund(self, refund_id: str) -> Refund:
        """Cancels a pending refund and releases its reservation of the deposit."""
        return self._call(
            lambda: cancel_refund.sync_detailed(refund_id, client=self._client),
            Refund,
        )

    def list_refunds(
        self,
        *,
        deposit: str | None = None,
        status: str | None = None,
        page_size: int = 100,
    ) -> Iterator[Refund]:
        """Yields the account's refunds, newest first, following every page."""
        return self._paginate(
            lambda starting_after: partial(
                list_refunds.sync_detailed,
                client=self._client,
                deposit=_unset(deposit),
                status=_unset(status),
                limit=page_size,
                starting_after=_unset(starting_after),
            ),
            RefundList,
        )

    def get_refund(self, refund_id: str, *, expand: list[str] | None = None) -> Refund:
        """Returns one refund; `expand` may name `deposit`."""
        return self._call(
            lambda: get_refund.sync_detailed(refund_id, client=self._client, expand=_unset(expand)),
            Refund,
        )

    def update_refund(self, refund_id: str, *, metadata: Metadata | None = None) -> Refund:
        """Merges `metadata` into the refund's."""
        body = UpdateMetadataRequest(metadata=_metadata(metadata))
        return self._call(
            lambda: update_refund.sync_detailed(refund_id, client=self._client, body=body),
            Refund,
        )

    def attestation(self, nonce: bytes) -> AttestationResponse:
        """Fetches attestation evidence binding `nonce` to the webhook keys of this key's account
        and mode.

        Raises `AttestationError` unless `report_data` binds `nonce`, the account, the mode, and
        every listed key. Verify the quote with the dstack verifier (`deploy/dstack-verifier.sh`),
        including that its report data is `report_data` zero-padded to 64 bytes, before pinning
        the keys.
        """
        response = self._call(
            lambda: get_attestation.sync_detailed(client=self._client, nonce=nonce.hex()),
            AttestationResponse,
        )
        verify_attestation_binding(response, nonce)
        return response

    def list_api_keys(self, *, page_size: int = 100) -> list[ApiKeyObject]:
        """The secret keys of this key's account and mode, newest first, without their secrets,
        from every page."""
        return list(
            self._paginate(
                lambda starting_after: partial(
                    list_api_keys.sync_detailed,
                    client=self._client,
                    limit=page_size,
                    starting_after=_unset(starting_after),
                ),
                ApiKeyList,
            )
        )

    def create_api_key(self, *, name: str = "") -> ApiKeyObject:
        """Creates a secret key of this mode; its `secret` is in this response only."""
        key = _idempotency_key(None)
        body = CreateApiKeyRequest(name=name)
        return self._call(
            lambda: create_api_key.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            ApiKeyObject,
        )

    def get_api_key(self, api_key_id: str) -> ApiKeyObject:
        """One secret key of this mode, without its secret."""
        return self._call(
            lambda: get_api_key.sync_detailed(api_key_id, client=self._client), ApiKeyObject
        )

    def roll_api_key(self, api_key_id: str, *, expires_in: int = 0) -> ApiKeyObject:
        """Replaces a key with a new one, returned with its `secret`; the old key keeps working
        for `expires_in` seconds (at most 7 days; `0` revokes it at once)."""
        key = _idempotency_key(None)
        body = RollApiKeyRequest(expires_in=expires_in)
        return self._call(
            lambda: roll_api_key.sync_detailed(
                api_key_id, client=self._client, body=body, idempotency_key=key
            ),
            ApiKeyObject,
        )

    def revoke_api_key(self, api_key_id: str) -> ApiKeyObject:
        """Revokes a key at once."""
        return self._call(
            lambda: revoke_api_key.sync_detailed(api_key_id, client=self._client), ApiKeyObject
        )

    def list_webhook_endpoints(self, *, page_size: int = 100) -> Iterator[WebhookEndpointObject]:
        """Yields the webhook endpoints of this mode, newest first."""
        return self._paginate(
            lambda starting_after: partial(
                list_webhook_endpoints.sync_detailed,
                client=self._client,
                limit=page_size,
                starting_after=_unset(starting_after),
            ),
            WebhookEndpointList,
        )

    def create_webhook_endpoint(
        self,
        url: str,
        enabled_events: list[str],
        *,
        description: str | None = None,
        metadata: Mapping[str, str] | None = None,
    ) -> WebhookEndpointObject:
        """Registers `url` for `enabled_events` (`["*"]` for all). Account events (`account.*`,
        `api_key.*`, `webhook_endpoint.*`) reach every enabled endpoint whatever it lists."""
        key = _idempotency_key(None)
        body = CreateWebhookEndpointRequest(
            url=url,
            enabled_events=enabled_events,
            description=_unset(description),
            metadata=_metadata(metadata),
        )
        return self._call(
            lambda: create_webhook_endpoint.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            WebhookEndpointObject,
        )

    def get_webhook_endpoint(self, endpoint_id: str) -> WebhookEndpointObject:
        """One webhook endpoint."""
        return self._call(
            lambda: get_webhook_endpoint.sync_detailed(endpoint_id, client=self._client),
            WebhookEndpointObject,
        )

    def update_webhook_endpoint(
        self,
        endpoint_id: str,
        *,
        url: str | None = None,
        enabled_events: list[str] | None = None,
        description: str | None = None,
        disabled: bool | None = None,
        metadata: Metadata | None = None,
    ) -> WebhookEndpointObject:
        """Updates the parameters given; `disabled=True` stops its deliveries. The endpoint is
        notified of its own change first."""
        key = _idempotency_key(None)
        body = UpdateWebhookEndpointRequest(
            url=_unset(url),
            enabled_events=_unset(enabled_events),
            description=_unset(description),
            disabled=_unset(disabled),
            metadata=_metadata(metadata),
        )
        return self._call(
            lambda: update_webhook_endpoint.sync_detailed(
                endpoint_id, client=self._client, body=body, idempotency_key=key
            ),
            WebhookEndpointObject,
        )

    def delete_webhook_endpoint(self, endpoint_id: str) -> DeletedWebhookEndpoint:
        """Deletes an endpoint; it receives the notice of its own deletion."""
        return self._call(
            lambda: delete_webhook_endpoint.sync_detailed(endpoint_id, client=self._client),
            DeletedWebhookEndpoint,
        )

    def test_webhook_endpoint(self, endpoint_id: str) -> EventObjectResponse:
        """Sends a test event to one endpoint and returns it."""
        key = _idempotency_key(None)
        return self._call(
            lambda: test_webhook_endpoint.sync_detailed(
                endpoint_id, client=self._client, idempotency_key=key
            ),
            EventObjectResponse,
        )

    def list_events(
        self,
        *,
        type: str | None = None,
        types: Sequence[str] | None = None,
        delivery_success: bool | None = None,
        created_gt: int | None = None,
        created_gte: int | None = None,
        created_lt: int | None = None,
        created_lte: int | None = None,
        page_size: int = 100,
    ) -> Iterator[EventObjectResponse]:
        """Yields the events of this mode, newest first: the account's audit log and every
        webhook ever sent. `type` filters by one event type, such as `deposit.credited`, or a
        group, `deposit.*`; `types` by up to 20. `delivery_success=False` yields the events a
        webhook endpoint has not received yet: resend them once it is fixed."""
        return self._paginate(
            lambda starting_after: partial(
                list_events.sync_detailed,
                client=self._client,
                type_=_unset(type),
                types=UNSET if types is None else list(types),
                delivery_success=_unset(delivery_success),
                createdgt=_unset(created_gt),
                createdgte=_unset(created_gte),
                createdlt=_unset(created_lt),
                createdlte=_unset(created_lte),
                limit=page_size,
                starting_after=_unset(starting_after),
            ),
            EventList,
        )

    def get_event(self, event_id: str) -> EventObjectResponse:
        """One event."""
        return self._call(
            lambda: get_event.sync_detailed(event_id, client=self._client), EventObjectResponse
        )

    def resend_event(self, event_id: str, *, webhook_endpoint: str) -> EventObjectResponse:
        """Delivers an event again to one enabled endpoint, as `stripe events resend` does."""
        key = _idempotency_key(None)
        body = ResendEventRequest(webhook_endpoint=webhook_endpoint)
        return self._call(
            lambda: resend_event.sync_detailed(
                event_id, client=self._client, body=body, idempotency_key=key
            ),
            EventObjectResponse,
        )

    def create_treasury_challenge(self, chain_id: int, address: str) -> TreasuryChallenge:
        """Returns the EIP-4361 message that proves `address` as the treasury of `chain_id`: an
        EOA signs it with `topup_sdk.sign_treasury_challenge` (or any wallet's `personal_sign`),
        a Safe's owners sign it as a Safe message (docs/integration.md)."""
        key = _idempotency_key(None)
        body = CreateTreasuryChallengeRequest(chain_id=chain_id, address=address)
        return self._call(
            lambda: create_treasury_challenge.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            TreasuryChallenge,
        )

    def create_treasury(self, chain_id: int, message: str, signature: str) -> Treasury:
        """Submits a signed challenge, announced as `treasury.created`. The chain's first treasury
        applies at once; a later live change is `pending` for 48 hours, then applies
        (`treasury.updated`) unless canceled (`treasury.canceled`)."""
        key = _idempotency_key(None)
        body = CreateTreasuryRequest(chain_id=chain_id, message=message, signature=signature)
        return self._call(
            lambda: create_treasury.sync_detailed(
                client=self._client, body=body, idempotency_key=key
            ),
            Treasury,
        )

    def list_treasuries(
        self, *, chain_id: int | None = None, status: str | None = None, page_size: int = 100
    ) -> list[Treasury]:
        """The treasuries of this mode, newest first, from every page."""
        return list(
            self._paginate(
                lambda starting_after: partial(
                    list_treasuries.sync_detailed,
                    client=self._client,
                    chain_id=_unset(chain_id),
                    status=_unset(status),
                    limit=page_size,
                    starting_after=_unset(starting_after),
                ),
                TreasuryList,
            )
        )

    def get_treasury(self, treasury_id: str) -> Treasury:
        """One treasury."""
        return self._call(
            lambda: get_treasury.sync_detailed(treasury_id, client=self._client), Treasury
        )

    def cancel_treasury(self, treasury_id: str) -> Treasury:
        """Cancels a pending treasury change before it applies."""
        key = _idempotency_key(None)
        return self._call(
            lambda: cancel_treasury.sync_detailed(
                treasury_id, client=self._client, idempotency_key=key
            ),
            Treasury,
        )

    def get_balance(self) -> Balance:
        """What the account's forwarders hold, per chain and token."""
        return self._call(lambda: get_balance.sync_detailed(client=self._client), Balance)

    def list_sweeps(
        self,
        *,
        chain_id: int | None = None,
        forwarder: str | None = None,
        token: str | None = None,
        page_size: int = 100,
    ) -> Iterator[Sweep]:
        """Yields the sweeps, finalized `Flushed` events of the account's forwarders, newest
        first."""
        return self._paginate(
            lambda starting_after: partial(
                list_sweeps.sync_detailed,
                client=self._client,
                chain_id=_unset(chain_id),
                forwarder=_unset(forwarder),
                token=_unset(token),
                limit=page_size,
                starting_after=_unset(starting_after),
            ),
            SweepList,
        )

    def list_forwarders(
        self,
        *,
        chain_id: int | None = None,
        sweepable: str | None = None,
        page_size: int = 100,
    ) -> Iterator[Forwarder]:
        """Yields the account's forwarders with the `(factory, salt, treasury)` each address
        derives from. With `sweepable` (a token contract), only those with a final unswept
        balance of it that may be swept: pass them to `topup_sdk.flush_transaction`."""
        return self._paginate(
            lambda starting_after: partial(
                list_forwarders.sync_detailed,
                client=self._client,
                chain_id=_unset(chain_id),
                sweepable=_unset(sweepable),
                limit=page_size,
                starting_after=_unset(starting_after),
            ),
            ForwarderList,
        )

    def _paginate(
        self, page: Callable[[str | None], Callable[[], Response[Any]]], kind: type[Any]
    ) -> Iterator[Any]:
        """Follows every page of a Stripe list, as Stripe's auto-pagination does."""
        starting_after: str | None = None
        while True:
            listed = self._call(page(starting_after), kind)
            yield from listed.data
            if not listed.has_more or not listed.data:
                return
            starting_after = listed.data[-1].id

    def _checked(self, quote: Quote) -> Quote:
        """Raises unless an open quote's address is the one derived from the pinned forwarder
        over the quote's treasury, and that treasury is the pinned one of its chain."""
        if self.forwarder is None or quote.status != "open":
            return quote
        self._check_treasury(quote.chain_id, quote.treasury, f"quote {quote.id}")
        factory, implementation = self.forwarder
        derived = quote_address(
            factory,
            implementation,
            quote.treasury,
            account=self.account_id(),
            client_reference_id=quote.client_reference_id,
            quote_id=quote.id,
        )
        if not same_address(derived, quote.address):
            raise AddressMismatchError(f"quote {quote.id} has an address the account cannot derive")
        return quote

    def _checked_deposit_address(self, address: DepositAddress) -> DepositAddress:
        """Raises unless every network of an active deposit address is the address derived from
        the pinned forwarder over that network's treasury, and each treasury is the pinned one
        of its chain. A retired address may pay a treasury since replaced, so it is not
        checked."""
        if self.forwarder is None or address.status != "active":
            return address
        factory, implementation = self.forwarder
        for network in address.networks:
            where = f"deposit address {address.id} on chain {network.chain_id}"
            self._check_treasury(network.chain_id, network.treasury, where)
            derived = deposit_address(
                factory,
                implementation,
                network.treasury,
                account=self.account_id(),
                livemode=address.livemode,
                client_reference_id=address.client_reference_id,
                version=address.version,
            )
            if not same_address(derived, network.address):
                raise AddressMismatchError(f"{where} is not one the account can derive")
        return address

    def _check_treasury(self, chain_id: int, treasury: str, where: str) -> None:
        if self.treasuries is None:
            return
        pinned = self.treasuries.get(chain_id)
        if pinned is None or not same_address(pinned, treasury):
            raise AddressMismatchError(f"{where} pays a treasury that is not the pinned one")

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
                # A saved response is the request's outcome; asking again replays it.
                replayed = response.headers.get("idempotent-replayed") == "true"
                if not retryable or replayed or attempt >= self._max_attempts:
                    raise error
                if error.retry_after is not None:
                    self._sleep(max(error.retry_after, self._initial_backoff))
                    attempt += 1
                    continue
            self._sleep(self._initial_backoff * 2 ** (attempt - 1))
            attempt += 1


def _idempotency_key(key: str | None) -> str:
    """One `Idempotency-Key` for every attempt of a request, generated unless given."""
    # The IETF Idempotency-Key header is an RFC 8941 string; the key is its content.
    return sf_string(key or str(uuid.uuid4()))


def _unset[V](value: V | None) -> V | Unset:
    return UNSET if value is None else value


def _metadata(metadata: Metadata | None) -> MetadataParamType0 | MetadataClear | Unset:
    if metadata is None:
        return UNSET
    if isinstance(metadata, str):
        if metadata:
            raise ValueError('metadata is a mapping of strings, or "" to unset every key')
        return ""
    return MetadataParamType0.from_dict(dict(metadata))


def _api_error(response: Response[Any]) -> ApiError:
    request_id = response.headers.get("request-id")
    retry_after = _seconds(response.headers.get("retry-after"))
    parsed = response.parsed
    if isinstance(parsed, ErrorResponse):
        error = parsed.error
        return ApiError(
            response.status_code,
            error.code,
            error.message,
            error_type=error.type_,
            param=error.param if isinstance(error.param, str) else None,
            doc_url=error.doc_url,
            request_id=request_id,
            retry_after=retry_after,
        )
    return ApiError(
        response.status_code,
        "unexpected_response",
        "undocumented response",
        request_id=request_id,
        retry_after=retry_after,
    )


def _seconds(value: str | None) -> float | None:
    """A `Retry-After` of delay seconds; `None` when absent or not a number of seconds."""
    if value is None or not value.isdigit():
        return None
    return float(value)
