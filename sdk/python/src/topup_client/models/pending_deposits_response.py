from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.pending_deposit_response import PendingDepositResponse


T = TypeVar("T", bound="PendingDepositsResponse")


@_attrs_define
class PendingDepositsResponse:
    """Pending transfers to an account's persistent addresses.

    Attributes:
        pending_deposits (list[PendingDepositResponse]): Pending transfers in block order.
    """

    pending_deposits: list[PendingDepositResponse]
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.pending_deposit_response import PendingDepositResponse  # noqa: PLC0415

        pending_deposits = []
        for pending_deposits_item_data in self.pending_deposits:
            pending_deposits_item = pending_deposits_item_data.to_dict()
            pending_deposits.append(pending_deposits_item)

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "pending_deposits": pending_deposits,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.pending_deposit_response import PendingDepositResponse  # noqa: PLC0415

        d = dict(src_dict)
        pending_deposits = []
        _pending_deposits = d.pop("pending_deposits")
        for pending_deposits_item_data in _pending_deposits:
            pending_deposits_item = PendingDepositResponse.from_dict(pending_deposits_item_data)

            pending_deposits.append(pending_deposits_item)

        pending_deposits_response = cls(
            pending_deposits=pending_deposits,
        )

        pending_deposits_response.additional_properties = d
        return pending_deposits_response

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
