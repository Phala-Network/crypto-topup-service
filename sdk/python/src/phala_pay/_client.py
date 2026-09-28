"""`PhalaPay`: the Phala Pay merchant API as Stripe-style resources over `TopupClient`."""

from __future__ import annotations

from collections.abc import Iterator, Mapping

import httpx

from topup_client.models import Config, Deposit, Quote, Refund
from topup_sdk import TopupClient
from topup_sdk.client import Metadata

from ._webhook import Webhook


class PhalaPay:
    """A client for one account and mode, authenticated with its secret key.

        pay = PhalaPay(api_base="https://pay.example.com", api_key=os.environ["PHALA_PAY_KEY"])
        quote = pay.quotes.create(account_id="team-42", amount=2500, chain_id=11155111,
                                  asset="pha")
        return {"client_secret": quote.client_secret}

    `api_key` is a secret key, `ppay_sk_test_…` or `ppay_sk_live_…`; the key selects the account
    and the mode. `forwarder`, the `(factory, implementation, treasury)` triple pinned from the
    attested deployment and the merchant's treasury, makes `quotes.create` and `quotes.retrieve`
    recompute every open quote's address and raise `AddressMismatchError` rather than return one
    the merchant did not derive; `account` (`acct_…`) saves the one `GET /v1/account` that check
    otherwise makes.

    Requests that fail with a transport error, `429`, or `5xx` are retried with backoff; `POST`s
    reuse one `Idempotency-Key` across retries, so a retry never creates a second object.
    """

    def __init__(
        self,
        api_base: str,
        api_key: str,
        *,
        account: str | None = None,
        forwarder: tuple[str, str, str] | None = None,
        timeout: float = 15.0,
        max_attempts: int = 4,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        self._client = TopupClient(
            api_base,
            api_key,
            account=account,
            forwarder=forwarder,
            timeout=timeout,
            max_attempts=max_attempts,
            transport=transport,
        )
        self.quotes = Quotes(self._client)
        self.deposits = Deposits(self._client)
        self.refunds = Refunds(self._client)
        self.config = ConfigResource(self._client)
        self.webhooks = Webhook

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> PhalaPay:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class Quotes:
    def __init__(self, client: TopupClient) -> None:
        self._client = client

    def create(
        self,
        *,
        account_id: str,
        amount: int,
        chain_id: int,
        asset: str,
        currency: str = "usd",
        idempotency_key: str | None = None,
        metadata: Mapping[str, str] | None = None,
    ) -> Quote:
        """Quotes `amount` cents for `account_id`, payable in `asset` on `chain_id`.

        Only this response carries `client_secret`, the value the payer's browser needs. Pass an
        `idempotency_key` of your own (for example your order id) to resume a checkout: the same
        key returns the same quote with a new `client_secret`, and the old one stops working.

        `metadata` is Stripe's: up to 50 string pairs for your own use, such as your order id,
        copied to the deposit that pays the quote. Do not store sensitive information in it.
        """
        return self._client.create_quote(
            account_id,
            amount,
            chain_id=chain_id,
            asset=asset,
            currency=currency,
            idempotency_key=idempotency_key,
            metadata=metadata,
        )

    def retrieve(self, quote_id: str) -> Quote:
        return self._client.get_quote(quote_id)

    def update(self, quote_id: str, *, metadata: Metadata | None = None) -> Quote:
        """Merges `metadata` into the quote's: a key set to `""` is unset, and `metadata=""`
        unsets every key."""
        return self._client.update_quote(quote_id, metadata=metadata)

    def cancel(self, quote_id: str) -> Quote:
        """Cancels an open quote no payment has reached; repeating it returns the canceled quote."""
        return self._client.cancel_quote(quote_id)


class Deposits:
    def __init__(self, client: TopupClient) -> None:
        self._client = client

    # Before `list`, whose name would shadow the builtin in later annotations.
    def retrieve(self, deposit_id: str, *, expand: list[str] | None = None) -> Deposit:
        return self._client.get_deposit(deposit_id, expand=expand)

    def update(self, deposit_id: str, *, metadata: Metadata | None = None) -> Deposit:
        """Merges `metadata` into the deposit's, which started as a copy of its quote's; the
        quote's is left unchanged."""
        return self._client.update_deposit(deposit_id, metadata=metadata)

    def list(
        self,
        *,
        account_id: str | None = None,
        quote: str | None = None,
        status: str | None = None,
        tx_hash: str | None = None,
        created_gte: int | None = None,
        created_lte: int | None = None,
        expand: list[str] | None = None,
    ) -> Iterator[Deposit]:
        """Yields every matching deposit, newest first, fetching pages as it goes."""
        return self._client.list_deposits(
            account_id=account_id,
            quote=quote,
            status=status,
            tx_hash=tx_hash,
            created_gte=created_gte,
            created_lte=created_lte,
            expand=expand,
        )


class Refunds:
    def __init__(self, client: TopupClient) -> None:
        self._client = client

    def create(
        self,
        *,
        deposit: str,
        destination_address: str,
        amount_atomic: int | None = None,
        idempotency_key: str | None = None,
        metadata: Mapping[str, str] | None = None,
    ) -> Refund:
        """Requests a refund of `deposit` (its unrefunded remainder unless `amount_atomic` is
        given) to an address the customer controls; finance approves and sends it."""
        return self._client.create_refund(
            deposit,
            destination_address,
            amount_atomic,
            idempotency_key=idempotency_key,
            metadata=metadata,
        )

    def retrieve(self, refund_id: str, *, expand: list[str] | None = None) -> Refund:
        return self._client.get_refund(refund_id, expand=expand)

    def update(self, refund_id: str, *, metadata: Metadata | None = None) -> Refund:
        """Merges `metadata` into the refund's."""
        return self._client.update_refund(refund_id, metadata=metadata)


class ConfigResource:
    def __init__(self, client: TopupClient) -> None:
        self._client = client

    def retrieve(self) -> Config:
        """The payable assets, limits, and quote terms, for the product's UI."""
        return self._client.get_config()
