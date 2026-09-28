from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.metadata_clear import check_metadata_clear
from ..models.metadata_clear import MetadataClear
from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.metadata_param_type_0 import MetadataParamType0


T = TypeVar("T", bound="CreateWebhookEndpointRequest")


@_attrs_define
class CreateWebhookEndpointRequest:
    """`POST /v1/webhook_endpoints` body.

    Example:
        {'description': 'Order fulfillment', 'enabled_events': ['deposit.credited', 'deposit.reversed',
            'refund.failed'], 'metadata': {'team': 'payments'}, 'url': 'https://example.com/phala-pay/webhooks'}

    Attributes:
        enabled_events (list[str]): The event types to deliver, such as `deposit.credited`, or `["*"]` for all.
        url (str): Where to deliver events, up to 2048 characters, without credentials or fragment: `https` on
            port 443; in test mode also `http` on port 80. Redirects are not followed.
        description (None | str | Unset): Your description, up to 5000 characters.
        metadata (MetadataClear | MetadataParamType0 | Unset): A `metadata` parameter: an object of string values, where
            `""` unsets the key, or `""` to
            unset every key.
    """

    enabled_events: list[str]
    url: str
    description: None | str | Unset = UNSET
    metadata: MetadataClear | MetadataParamType0 | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        enabled_events = self.enabled_events

        url = self.url

        description: None | str | Unset
        if isinstance(self.description, Unset):
            description = UNSET
        else:
            description = self.description

        metadata: dict[str, Any] | str | Unset
        if isinstance(self.metadata, Unset):
            metadata = UNSET
        elif isinstance(self.metadata, MetadataParamType0):
            metadata = self.metadata.to_dict()
        else:
            metadata = self.metadata

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "enabled_events": enabled_events,
                "url": url,
            }
        )
        if description is not UNSET:
            field_dict["description"] = description
        if metadata is not UNSET:
            field_dict["metadata"] = metadata

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        d = dict(src_dict)
        enabled_events = cast(list[str], d.pop("enabled_events"))

        url = d.pop("url")

        def _parse_description(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        description = _parse_description(d.pop("description", UNSET))

        def _parse_metadata(data: object) -> MetadataClear | MetadataParamType0 | Unset:
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_metadata_param_type_0 = MetadataParamType0.from_dict(data)

                return componentsschemas_metadata_param_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            if not isinstance(data, str):
                raise TypeError()
            componentsschemas_metadata_param_type_1 = check_metadata_clear(data)

            return componentsschemas_metadata_param_type_1

        metadata = _parse_metadata(d.pop("metadata", UNSET))

        create_webhook_endpoint_request = cls(
            enabled_events=enabled_events,
            url=url,
            description=description,
            metadata=metadata,
        )

        return create_webhook_endpoint_request
