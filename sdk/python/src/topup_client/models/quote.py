from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.quote_payment import QuotePayment


T = TypeVar("T", bound="Quote")


@_attrs_define
class Quote:
    """A quote: a locked price, an exact token amount, and a single-use address to pay it to.

    Attributes:
        account_id (str): Your account identifier.
        address (str): Single-use forwarder address to pay.
        amount (int): Credit in the currency's minor unit.
        amount_atomic (str): The exact token amount to pay, in base units, as a decimal string.
        asset (str): Asset code.
        chain_id (int): EVM chain identifier.
        created (int): Creation time, Unix seconds.
        currency (str): `usd`.
        exchange_rate (str): The locked price in USD per token, a decimal string with 8 decimal places.
        expires_at (int): End of the payment window, Unix seconds.
        id (str): `qt_` id. New quotes' address salt is `keccak256(abi.encode(product_slug, account_id,
            "lock", id))`.
        object_ (str): Always `quote`.
        payment_uri (str): EIP-681 URI carrying the token, chain, address, and amount.
        status (str): `open`, `complete` (a matching payment consumed it), `expired`, or `canceled`. A quote stays
            `open` after `expires_at` until the finalized chain passes it, so a payment mined in time
            is never reported as expired; hide the address once `expires_at` has passed.
        deposit (None | str | Unset): `dep_` id of the deposit that completed the quote.
        payment (None | QuotePayment | Unset):
    """

    account_id: str
    address: str
    amount: int
    amount_atomic: str
    asset: str
    chain_id: int
    created: int
    currency: str
    exchange_rate: str
    expires_at: int
    id: str
    object_: str
    payment_uri: str
    status: str
    deposit: None | str | Unset = UNSET
    payment: None | QuotePayment | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.quote_payment import QuotePayment  # noqa: PLC0415

        account_id = self.account_id

        address = self.address

        amount = self.amount

        amount_atomic = self.amount_atomic

        asset = self.asset

        chain_id = self.chain_id

        created = self.created

        currency = self.currency

        exchange_rate = self.exchange_rate

        expires_at = self.expires_at

        id = self.id

        object_ = self.object_

        payment_uri = self.payment_uri

        status = self.status

        deposit: None | str | Unset
        if isinstance(self.deposit, Unset):
            deposit = UNSET
        else:
            deposit = self.deposit

        payment: dict[str, Any] | None | Unset
        if isinstance(self.payment, Unset):
            payment = UNSET
        elif isinstance(self.payment, QuotePayment):
            payment = self.payment.to_dict()
        else:
            payment = self.payment

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "account_id": account_id,
                "address": address,
                "amount": amount,
                "amount_atomic": amount_atomic,
                "asset": asset,
                "chain_id": chain_id,
                "created": created,
                "currency": currency,
                "exchange_rate": exchange_rate,
                "expires_at": expires_at,
                "id": id,
                "object": object_,
                "payment_uri": payment_uri,
                "status": status,
            }
        )
        if deposit is not UNSET:
            field_dict["deposit"] = deposit
        if payment is not UNSET:
            field_dict["payment"] = payment

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.quote_payment import QuotePayment  # noqa: PLC0415

        d = dict(src_dict)
        account_id = d.pop("account_id")

        address = d.pop("address")

        amount = d.pop("amount")

        amount_atomic = d.pop("amount_atomic")

        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        created = d.pop("created")

        currency = d.pop("currency")

        exchange_rate = d.pop("exchange_rate")

        expires_at = d.pop("expires_at")

        id = d.pop("id")

        object_ = d.pop("object")

        payment_uri = d.pop("payment_uri")

        status = d.pop("status")

        def _parse_deposit(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        deposit = _parse_deposit(d.pop("deposit", UNSET))

        def _parse_payment(data: object) -> None | QuotePayment | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                payment_type_0 = QuotePayment.from_dict(data)

                return payment_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(None | QuotePayment | Unset, data)

        payment = _parse_payment(d.pop("payment", UNSET))

        quote = cls(
            account_id=account_id,
            address=address,
            amount=amount,
            amount_atomic=amount_atomic,
            asset=asset,
            chain_id=chain_id,
            created=created,
            currency=currency,
            exchange_rate=exchange_rate,
            expires_at=expires_at,
            id=id,
            object_=object_,
            payment_uri=payment_uri,
            status=status,
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
