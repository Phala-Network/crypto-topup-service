from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.treasury_list_object import check_treasury_list_object
from ..models.treasury_list_object import TreasuryListObject
from typing import cast

if TYPE_CHECKING:
    from ..models.treasury import Treasury


T = TypeVar("T", bound="TreasuryList")


@_attrs_define
class TreasuryList:
    """`GET /v1/treasuries` response.

    Example:
        {'data': [{'address': '0x936c1991f8da9a919fa11b557a3514719f5a4504', 'canceled_at': None, 'cancellation_reason':
            None, 'chain_id': 1, 'created': 1790467200, 'effective_at': 1790467200, 'id':
            'trs_4d8a2c6e0b1f47a3c5e7d9b1a3c5e7f9', 'kind': 'contract', 'livemode': False, 'object': 'treasury',
            'replaced_at': None, 'status': 'active'}], 'has_more': False, 'object': 'list', 'url': '/v1/treasuries'}

    Attributes:
        data (list[Treasury]): The mode's treasuries, newest first.
        has_more (bool): Whether more treasuries match than `limit`.
        object_ (TreasuryListObject): Always `list`.
        url (str): The list's path, `/v1/treasuries`.
    """

    data: list[Treasury]
    has_more: bool
    object_: TreasuryListObject
    url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.treasury import Treasury  # noqa: PLC0415

        data = []
        for data_item_data in self.data:
            data_item = data_item_data.to_dict()
            data.append(data_item)

        has_more = self.has_more

        object_: str = self.object_

        url = self.url

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "data": data,
                "has_more": has_more,
                "object": object_,
                "url": url,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.treasury import Treasury  # noqa: PLC0415

        d = dict(src_dict)
        data = []
        _data = d.pop("data")
        for data_item_data in _data:
            data_item = Treasury.from_dict(data_item_data)

            data.append(data_item)

        has_more = d.pop("has_more")

        object_ = check_treasury_list_object(d.pop("object"))

        url = d.pop("url")

        treasury_list = cls(
            data=data,
            has_more=has_more,
            object_=object_,
            url=url,
        )

        treasury_list.additional_properties = d
        return treasury_list

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
