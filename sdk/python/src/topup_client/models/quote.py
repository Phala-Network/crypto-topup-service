from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.deposit import Deposit
    from ..models.payment import Payment
    from ..models.quote_metadata import QuoteMetadata


T = TypeVar("T", bound="Quote")


@_attrs_define
class Quote:
    """A quote: a locked price, an exact token amount, and a single-use address to pay it to.

    Attributes:
        address (str): Single-use forwarder address to pay.
        amount (int): Credit in the currency's minor unit.
        amount_atomic (str): The exact token amount to pay, in base units, as a decimal string.
        asset (str): Asset code.
        chain_id (int): EVM chain identifier.
        client_reference_id (str): Your identifier of the customer the quote credits.
        created (int): Creation time, Unix seconds.
        currency (str): `usd`.
        exchange_rate (str): The locked price in USD per token, a decimal string with 8 decimal places.
        expires_at (int): End of the payment window, Unix seconds.
        id (str): `qt_` id. The quote's address salt is `keccak256(abi.encode(account,
            client_reference_id, "lock", id))` with the types `(string, string, string, string)`,
            where `account` is your `acct_` id.
        livemode (bool): Whether the quote was created with a live key.
        metadata (QuoteMetadata): Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)); `{}` when
            none.
        object_ (str): Always `quote`.
        payment_uri (str): EIP-681 URI carrying the token, chain, address, and amount.
        status (str): `open`, `complete` (a matching payment consumed it), `expired`, or `canceled`. A quote stays
            `open` after `expires_at` until the finalized chain passes it, so a payment mined in time
            is never reported as expired; hide the address once `expires_at` has passed.
        treasury (str): The treasury the address pays: your treasury of the chain when the quote was created. The
            address is the factory's `CREATE2` over it and the salt.
        client_secret (None | str | Unset): Lets the payer's browser read the quote's public view, `ClientQuote`, from
            `GET /v1/quotes/{id}?client_secret=…` without an API key. Returned only by
            `POST /v1/quotes`; a repeat with the same `Idempotency-Key` within 24 hours replays the
            first response, the same secret included. Give it only to the paying customer's page, and
            do not log it.
        deposit (Deposit | None | str | Unset):
        payment (None | Payment | Unset):
    """

    address: str
    amount: int
    amount_atomic: str
    asset: str
    chain_id: int
    client_reference_id: str
    created: int
    currency: str
    exchange_rate: str
    expires_at: int
    id: str
    livemode: bool
    metadata: QuoteMetadata
    object_: str
    payment_uri: str
    status: str
    treasury: str
    client_secret: None | str | Unset = UNSET
    deposit: Deposit | None | str | Unset = UNSET
    payment: None | Payment | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit import Deposit  # noqa: PLC0415
        from ..models.payment import Payment  # noqa: PLC0415
        from ..models.quote_metadata import QuoteMetadata  # noqa: PLC0415

        address = self.address

        amount = self.amount

        amount_atomic = self.amount_atomic

        asset = self.asset

        chain_id = self.chain_id

        client_reference_id = self.client_reference_id

        created = self.created

        currency = self.currency

        exchange_rate = self.exchange_rate

        expires_at = self.expires_at

        id = self.id

        livemode = self.livemode

        metadata = self.metadata.to_dict()

        object_ = self.object_

        payment_uri = self.payment_uri

        status = self.status

        treasury = self.treasury

        client_secret: None | str | Unset
        if isinstance(self.client_secret, Unset):
            client_secret = UNSET
        else:
            client_secret = self.client_secret

        deposit: dict[str, Any] | None | str | Unset
        if isinstance(self.deposit, Unset):
            deposit = UNSET
        elif isinstance(self.deposit, Deposit):
            deposit = self.deposit.to_dict()
        else:
            deposit = self.deposit

        payment: dict[str, Any] | None | Unset
        if isinstance(self.payment, Unset):
            payment = UNSET
        elif isinstance(self.payment, Payment):
            payment = self.payment.to_dict()
        else:
            payment = self.payment

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "amount": amount,
                "amount_atomic": amount_atomic,
                "asset": asset,
                "chain_id": chain_id,
                "client_reference_id": client_reference_id,
                "created": created,
                "currency": currency,
                "exchange_rate": exchange_rate,
                "expires_at": expires_at,
                "id": id,
                "livemode": livemode,
                "metadata": metadata,
                "object": object_,
                "payment_uri": payment_uri,
                "status": status,
                "treasury": treasury,
            }
        )
        if client_secret is not UNSET:
            field_dict["client_secret"] = client_secret
        if deposit is not UNSET:
            field_dict["deposit"] = deposit
        if payment is not UNSET:
            field_dict["payment"] = payment

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit import Deposit  # noqa: PLC0415
        from ..models.payment import Payment  # noqa: PLC0415
        from ..models.quote_metadata import QuoteMetadata  # noqa: PLC0415

        d = dict(src_dict)
        address = d.pop("address")

        amount = d.pop("amount")

        amount_atomic = d.pop("amount_atomic")

        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        client_reference_id = d.pop("client_reference_id")

        created = d.pop("created")

        currency = d.pop("currency")

        exchange_rate = d.pop("exchange_rate")

        expires_at = d.pop("expires_at")

        id = d.pop("id")

        livemode = d.pop("livemode")

        metadata = QuoteMetadata.from_dict(d.pop("metadata"))

        object_ = d.pop("object")

        payment_uri = d.pop("payment_uri")

        status = d.pop("status")

        treasury = d.pop("treasury")

        def _parse_client_secret(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        client_secret = _parse_client_secret(d.pop("client_secret", UNSET))

        def _parse_deposit(data: object) -> Deposit | None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_expandable_deposit_type_1 = Deposit.from_dict(data)

                return componentsschemas_expandable_deposit_type_1
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(Deposit | None | str | Unset, data)

        deposit = _parse_deposit(d.pop("deposit", UNSET))

        def _parse_payment(data: object) -> None | Payment | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                payment_type_0 = Payment.from_dict(data)

                return payment_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(None | Payment | Unset, data)

        payment = _parse_payment(d.pop("payment", UNSET))

        quote = cls(
            address=address,
            amount=amount,
            amount_atomic=amount_atomic,
            asset=asset,
            chain_id=chain_id,
            client_reference_id=client_reference_id,
            created=created,
            currency=currency,
            exchange_rate=exchange_rate,
            expires_at=expires_at,
            id=id,
            livemode=livemode,
            metadata=metadata,
            object_=object_,
            payment_uri=payment_uri,
            status=status,
            treasury=treasury,
            client_secret=client_secret,
            deposit=deposit,
            payment=payment,
        )

        quote.additional_properties = d
        return quote

    @property
    def additional_keys(self) -> list[str]:
        return list(self.additional_properties.keys())

    def __getitem__(self, key: str) -> Any:
        return self.additional_properties[key]

    def __setitem__(self, key: str, value: Any) -> None:
        self.additional_properties[key] = value

    def __delitem__(self, key: str) -> None:
        del self.additional_properties[key]

    def __contains__(self, key: str) -> bool:
        return key in self.additional_properties
