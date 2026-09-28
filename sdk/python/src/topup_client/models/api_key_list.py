from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.api_key_list_object import ApiKeyListObject
from ..models.api_key_list_object import check_api_key_list_object
from typing import cast

if TYPE_CHECKING:
    from ..models.api_key_object import ApiKeyObject


T = TypeVar("T", bound="ApiKeyList")


@_attrs_define
class ApiKeyList:
    """`GET /v1/api_keys` response.

    Example:
        {'data': [{'created': 1790467200, 'expires_at': None, 'id': 'key_6a8c0e2b4d1f43a5c7e9b1d3f5a7c9e1', 'last_used':
            1790553600, 'livemode': False, 'name': 'fulfillment worker', 'object': 'api_key', 'permissions':
            ['account.read', 'deposit_addresses.read', 'deposit_addresses.write', 'deposits.read', 'events.read',
            'quotes.read', 'quotes.write', 'refunds.read'], 'redacted': 'ppay_rk_test_…Yz4x', 'status': 'active', 'type':
            'restricted'}], 'has_more': False, 'object': 'list', 'url': '/v1/api_keys'}

    Attributes:
        data (list[ApiKeyObject]): The mode's keys, newest first.
        has_more (bool): Always `false`: every key of the mode is listed.
        object_ (ApiKeyListObject): Always `list`.
        url (str): The list's path, `/v1/api_keys`.
    """

    data: list[ApiKeyObject]
    has_more: bool
    object_: ApiKeyListObject
    url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.api_key_object import ApiKeyObject  # noqa: PLC0415

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
        from ..models.api_key_object import ApiKeyObject  # noqa: PLC0415

        d = dict(src_dict)
        data = []
        _data = d.pop("data")
        for data_item_data in _data:
            data_item = ApiKeyObject.from_dict(data_item_data)

            data.append(data_item)

        has_more = d.pop("has_more")

        object_ = check_api_key_list_object(d.pop("object"))

        url = d.pop("url")

        api_key_list = cls(
            data=data,
            has_more=has_more,
            object_=object_,
            url=url,
        )

        api_key_list.additional_properties = d
        return api_key_list

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
