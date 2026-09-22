from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from uuid import UUID


T = TypeVar("T", bound="RefundResponse")


@_attrs_define
class RefundResponse:
    """Customer refund request accepted for finance review.

    Attributes:
        amount_atomic (str): Atomic token amount.
        deposit_id (UUID): Related deposit identifier.
        id (UUID): Refund request identifier.
        status (str): Stable workflow status.
        to_address (str): Customer-controlled destination address.
    """

    amount_atomic: str
    deposit_id: UUID
    id: UUID
    status: str
    to_address: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        amount_atomic = self.amount_atomic

        deposit_id = str(self.deposit_id)

        id = str(self.id)

        status = self.status

        to_address = self.to_address

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "deposit_id": deposit_id,
                "id": id,
                "status": status,
                "to_address": to_address,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        amount_atomic = d.pop("amount_atomic")

        deposit_id = UUID(d.pop("deposit_id"))

        id = UUID(d.pop("id"))

        status = d.pop("status")

        to_address = d.pop("to_address")

        refund_response = cls(
            amount_atomic=amount_atomic,
            deposit_id=deposit_id,
            id=id,
            status=status,
            to_address=to_address,
        )

        refund_response.additional_properties = d
        return refund_response

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
