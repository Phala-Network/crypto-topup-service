from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="CreateRefundRequest")


@_attrs_define
class CreateRefundRequest:
    """`POST /v1/refunds` body.

    Attributes:
        deposit (str): `dep_` id of the deposit to refund.
        destination_address (str): Address the customer controls; never default it to the sender, which may be an
            exchange.
        amount_atomic (None | str | Unset): Amount in base units, as a decimal string; the unrefunded remainder when
            absent.
    """

    deposit: str
    destination_address: str
    amount_atomic: None | str | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        deposit = self.deposit

        destination_address = self.destination_address

        amount_atomic: None | str | Unset
        if isinstance(self.amount_atomic, Unset):
            amount_atomic = UNSET
        else:
            amount_atomic = self.amount_atomic

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "deposit": deposit,
                "destination_address": destination_address,
            }
        )
        if amount_atomic is not UNSET:
            field_dict["amount_atomic"] = amount_atomic

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        deposit = d.pop("deposit")

        destination_address = d.pop("destination_address")

        def _parse_amount_atomic(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        amount_atomic = _parse_amount_atomic(d.pop("amount_atomic", UNSET))

        create_refund_request = cls(
            deposit=deposit,
            destination_address=destination_address,
            amount_atomic=amount_atomic,
        )

        return create_refund_request
