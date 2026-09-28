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


T = TypeVar("T", bound="UpdateWebhookEndpointRequest")


@_attrs_define
class UpdateWebhookEndpointRequest:
    """`POST /v1/webhook_endpoints/{id}` body; parameters not sent are left unchanged.

    Example:
        {'disabled': False, 'enabled_events': ['*']}

    Attributes:
        description (None | str | Unset): A new description; `""` unsets it.
        disabled (bool | None | Unset): `true` disables the endpoint, `false` enables it. A disabled endpoint receives
            nothing and
            its pending deliveries stop; resend missed events with `POST /v1/events/{id}/resend`.
        enabled_events (list[str] | None | Unset): New event types, or `["*"]`.
        metadata (MetadataClear | MetadataParamType0 | Unset): A `metadata` parameter: an object of string values, where
            `""` unsets the key, or `""` to
            unset every key.
        url (None | str | Unset): A new URL, as on creation.
    """

    description: None | str | Unset = UNSET
    disabled: bool | None | Unset = UNSET
    enabled_events: list[str] | None | Unset = UNSET
    metadata: MetadataClear | MetadataParamType0 | Unset = UNSET
    url: None | str | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        description: None | str | Unset
        if isinstance(self.description, Unset):
            description = UNSET
        else:
            description = self.description

        disabled: bool | None | Unset
        if isinstance(self.disabled, Unset):
            disabled = UNSET
        else:
            disabled = self.disabled

        enabled_events: list[str] | None | Unset
        if isinstance(self.enabled_events, Unset):
            enabled_events = UNSET
        elif isinstance(self.enabled_events, list):
            enabled_events = self.enabled_events

        else:
            enabled_events = self.enabled_events

        metadata: dict[str, Any] | str | Unset
        if isinstance(self.metadata, Unset):
            metadata = UNSET
        elif isinstance(self.metadata, MetadataParamType0):
            metadata = self.metadata.to_dict()
        else:
            metadata = self.metadata

        url: None | str | Unset
        if isinstance(self.url, Unset):
            url = UNSET
        else:
            url = self.url

        field_dict: dict[str, Any] = {}

        field_dict.update({})
        if description is not UNSET:
            field_dict["description"] = description
        if disabled is not UNSET:
            field_dict["disabled"] = disabled
        if enabled_events is not UNSET:
            field_dict["enabled_events"] = enabled_events
        if metadata is not UNSET:
            field_dict["metadata"] = metadata
        if url is not UNSET:
            field_dict["url"] = url

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        d = dict(src_dict)

        def _parse_description(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        description = _parse_description(d.pop("description", UNSET))

        def _parse_disabled(data: object) -> bool | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(bool | None | Unset, data)

        disabled = _parse_disabled(d.pop("disabled", UNSET))

        def _parse_enabled_events(data: object) -> list[str] | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, list):
                    raise TypeError()
                enabled_events_type_0 = cast(list[str], data)

                return enabled_events_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(list[str] | None | Unset, data)

        enabled_events = _parse_enabled_events(d.pop("enabled_events", UNSET))

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

        def _parse_url(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        url = _parse_url(d.pop("url", UNSET))

        update_webhook_endpoint_request = cls(
            description=description,
            disabled=disabled,
            enabled_events=enabled_events,
            metadata=metadata,
            url=url,
        )

        return update_webhook_endpoint_request
