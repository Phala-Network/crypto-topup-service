from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.webhook_endpoint_list_object import check_webhook_endpoint_list_object
from ..models.webhook_endpoint_list_object import WebhookEndpointListObject
from typing import cast

if TYPE_CHECKING:
    from ..models.webhook_endpoint_object import WebhookEndpointObject


T = TypeVar("T", bound="WebhookEndpointList")


@_attrs_define
class WebhookEndpointList:
    """A page of webhook endpoints, newest first (<https://docs.stripe.com/api/pagination>).

    Example:
        {'data': [{'created': 1790467200, 'description': 'Order fulfillment', 'disabled_reason': None, 'enabled_events':
            ['deposit.credited', 'deposit.reversed', 'refund.failed'], 'id': 'we_9e1c3a5b7d2f40c6e8a0b2d4f6a8c0e1',
            'last_attempt': {'at': 1790553695, 'status_code': 503}, 'livemode': False, 'metadata': {'team': 'payments'},
            'object': 'webhook_endpoint', 'oldest_pending_at': 1790553630, 'pending_deliveries': 2, 'status': 'enabled',
            'url': 'https://example.com/phala-pay/webhooks'}], 'has_more': False, 'object': 'list', 'url':
            '/v1/webhook_endpoints'}

    Attributes:
        data (list[WebhookEndpointObject]): The endpoints.
        has_more (bool): Whether more endpoints follow in the direction of this page.
        object_ (WebhookEndpointListObject): Always `list`.
        url (str): The list's path, `/v1/webhook_endpoints`.
    """

    data: list[WebhookEndpointObject]
    has_more: bool
    object_: WebhookEndpointListObject
    url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.webhook_endpoint_object import WebhookEndpointObject  # noqa: PLC0415

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
        from ..models.webhook_endpoint_object import WebhookEndpointObject  # noqa: PLC0415

        d = dict(src_dict)
        data = []
        _data = d.pop("data")
        for data_item_data in _data:
            data_item = WebhookEndpointObject.from_dict(data_item_data)

            data.append(data_item)

        has_more = d.pop("has_more")

        object_ = check_webhook_endpoint_list_object(d.pop("object"))

        url = d.pop("url")

        webhook_endpoint_list = cls(
            data=data,
            has_more=has_more,
            object_=object_,
            url=url,
        )

        webhook_endpoint_list.additional_properties = d
        return webhook_endpoint_list

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
