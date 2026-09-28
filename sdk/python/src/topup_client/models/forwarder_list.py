from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.forwarder_list_object import check_forwarder_list_object
from ..models.forwarder_list_object import ForwarderListObject
from typing import cast

if TYPE_CHECKING:
    from ..models.forwarder import Forwarder


T = TypeVar("T", bound="ForwarderList")


@_attrs_define
class ForwarderList:
    """A page of forwarders (<https://docs.stripe.com/api/pagination>), in `id` order.

    Example:
        {'data': [{'address': '0x2f3e91325b2288bce392711f85f5359661062a91', 'chain_id': 1, 'deposit_address': None,
            'factory': '0x9e5f1d3c7a2b4e6f8a0c1d3e5f7a9b2c4d6e8f01', 'id': 'fwd_5c7e9a1b3d2f44c6e8a0b2d4f6c8e0a2',
            'livemode': False, 'object': 'forwarder', 'quote': 'qt_5f1c0b6a2d9e4f3a8b7c6d5e4f3a2b10', 'salt':
            '0x4e9767dd0c2ab5b953a305c3f10dc1e0d1f7c9d3cbab8463509d2edb06ca4b52', 'superseded_at': None, 'treasury':
            '0x936c1991f8da9a919fa11b557a3514719f5a4504'}], 'has_more': False, 'object': 'list', 'url': '/v1/forwarders'}

    Attributes:
        data (list[Forwarder]): The forwarders.
        has_more (bool): Whether more forwarders follow in the direction of this page.
        object_ (ForwarderListObject): Always `list`.
        url (str): The list's path, `/v1/forwarders`.
    """

    data: list[Forwarder]
    has_more: bool
    object_: ForwarderListObject
    url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.forwarder import Forwarder  # noqa: PLC0415

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
        from ..models.forwarder import Forwarder  # noqa: PLC0415

        d = dict(src_dict)
        data = []
        _data = d.pop("data")
        for data_item_data in _data:
            data_item = Forwarder.from_dict(data_item_data)

            data.append(data_item)

        has_more = d.pop("has_more")

        object_ = check_forwarder_list_object(d.pop("object"))

        url = d.pop("url")

        forwarder_list = cls(
            data=data,
            has_more=has_more,
            object_=object_,
            url=url,
        )

        forwarder_list.additional_properties = d
        return forwarder_list

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
