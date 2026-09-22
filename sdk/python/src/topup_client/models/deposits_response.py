from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast
from uuid import UUID

if TYPE_CHECKING:
    from ..models.deposit_response import DepositResponse


T = TypeVar("T", bound="DepositsResponse")


@_attrs_define
class DepositsResponse:
    """A page of deposits.

    Attributes:
        deposits (list[DepositResponse]): Deposits in descending creation order.
        next_cursor (None | Unset | UUID): Cursor for the next page, or `null` when exhausted.
    """

    deposits: list[DepositResponse]
    next_cursor: None | Unset | UUID = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit_response import DepositResponse  # noqa: PLC0415

        deposits = []
        for deposits_item_data in self.deposits:
            deposits_item = deposits_item_data.to_dict()
            deposits.append(deposits_item)

        next_cursor: None | str | Unset
        if isinstance(self.next_cursor, Unset):
            next_cursor = UNSET
        elif isinstance(self.next_cursor, UUID):
            next_cursor = str(self.next_cursor)
        else:
            next_cursor = self.next_cursor

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "deposits": deposits,
            }
        )
        if next_cursor is not UNSET:
            field_dict["next_cursor"] = next_cursor

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit_response import DepositResponse  # noqa: PLC0415

        d = dict(src_dict)
        deposits = []
        _deposits = d.pop("deposits")
        for deposits_item_data in _deposits:
            deposits_item = DepositResponse.from_dict(deposits_item_data)

            deposits.append(deposits_item)

        def _parse_next_cursor(data: object) -> None | Unset | UUID:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, str):
                    raise TypeError()
                next_cursor_type_0 = UUID(data)

                return next_cursor_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(None | Unset | UUID, data)

        next_cursor = _parse_next_cursor(d.pop("next_cursor", UNSET))

        deposits_response = cls(
            deposits=deposits,
            next_cursor=next_cursor,
        )

        deposits_response.additional_properties = d
        return deposits_response

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
