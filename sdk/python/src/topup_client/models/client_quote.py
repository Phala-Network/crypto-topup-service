from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="ClientQuote")


@_attrs_define
class ClientQuote:
    """The public view of a quote, read with its `client_secret` and without a signature, for the
    payer's checkout page. It has no account or internal fields.

        Attributes:
            address (str): Single-use forwarder address to pay.
            amount (int): Credit in the currency's minor unit.
            amount_atomic (str): The exact token amount to pay, in base units, as a decimal string.
            asset (str): Asset code.
            chain_id (int): EVM chain identifier.
            currency (str): `usd`.
            decimals (int): The token's decimals, to display `amount_atomic`.
            expires_at (int): End of the payment window, Unix seconds.
            id (str): `qt_` id.
            object_ (str): Always `quote`.
            payment_status (str): Progress of the payment shown on the page; display only, never a reason to deliver
                anything: `none`; `seen` (in a block that is not final yet and may still disappear);
                `confirming` (final, being valued and screened); `credited`; or `rejected` (final and not
                credited; the payer should contact the product's support).
            payment_uri (str): EIP-681 URI carrying the token, chain, address, and amount.
            status (str): `open`, `complete`, `expired`, or `canceled`, as on `Quote`; hide the address once
                `expires_at` has passed.
            confirmations (int | None | Unset): While `seen`: blocks on top of and including the payment's block; otherwise
                `null`.
    """

    address: str
    amount: int
    amount_atomic: str
    asset: str
    chain_id: int
    currency: str
    decimals: int
    expires_at: int
    id: str
    object_: str
    payment_status: str
    payment_uri: str
    status: str
    confirmations: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        amount = self.amount

        amount_atomic = self.amount_atomic

        asset = self.asset

        chain_id = self.chain_id

        currency = self.currency

        decimals = self.decimals

        expires_at = self.expires_at

        id = self.id

        object_ = self.object_

        payment_status = self.payment_status

        payment_uri = self.payment_uri

        status = self.status

        confirmations: int | None | Unset
        if isinstance(self.confirmations, Unset):
            confirmations = UNSET
        else:
            confirmations = self.confirmations

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "amount": amount,
                "amount_atomic": amount_atomic,
                "asset": asset,
                "chain_id": chain_id,
                "currency": currency,
                "decimals": decimals,
                "expires_at": expires_at,
                "id": id,
                "object": object_,
                "payment_status": payment_status,
                "payment_uri": payment_uri,
                "status": status,
            }
        )
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        amount = d.pop("amount")

        amount_atomic = d.pop("amount_atomic")

        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        currency = d.pop("currency")

        decimals = d.pop("decimals")

        expires_at = d.pop("expires_at")

        id = d.pop("id")

        object_ = d.pop("object")

        payment_status = d.pop("payment_status")

        payment_uri = d.pop("payment_uri")

        status = d.pop("status")

        def _parse_confirmations(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        confirmations = _parse_confirmations(d.pop("confirmations", UNSET))

        client_quote = cls(
            address=address,
            amount=amount,
            amount_atomic=amount_atomic,
            asset=asset,
            chain_id=chain_id,
            currency=currency,
            decimals=decimals,
            expires_at=expires_at,
            id=id,
            object_=object_,
            payment_status=payment_status,
            payment_uri=payment_uri,
            status=status,
            confirmations=confirmations,
        )

        client_quote.additional_properties = d
        return client_quote

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
