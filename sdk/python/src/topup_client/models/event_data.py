from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.event_data_object import EventDataObject
    from ..models.event_data_previous_attributes_type_0 import EventDataPreviousAttributesType0


T = TypeVar("T", bound="EventData")


@_attrs_define
class EventData:
    """An event's `data`.

    Attributes:
        object_ (EventDataObject): The object's API representation when the event was created: a deposit, quote, refund,
            API key, treasury, webhook endpoint, or the account, as its `GET` returned it then.
        previous_attributes (EventDataPreviousAttributesType0 | None | Unset): On `*.updated` events: the fields that
            changed, with their values before the change (a
            changed `metadata` holds only its changed keys; a field that was added is `null`).
    """

    object_: EventDataObject
    previous_attributes: EventDataPreviousAttributesType0 | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.event_data_object import EventDataObject  # noqa: PLC0415
        from ..models.event_data_previous_attributes_type_0 import EventDataPreviousAttributesType0  # noqa: PLC0415

        object_ = self.object_.to_dict()

        previous_attributes: dict[str, Any] | None | Unset
        if isinstance(self.previous_attributes, Unset):
            previous_attributes = UNSET
        elif isinstance(self.previous_attributes, EventDataPreviousAttributesType0):
            previous_attributes = self.previous_attributes.to_dict()
        else:
            previous_attributes = self.previous_attributes

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "object": object_,
            }
        )
        if previous_attributes is not UNSET:
            field_dict["previous_attributes"] = previous_attributes

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.event_data_object import EventDataObject  # noqa: PLC0415
        from ..models.event_data_previous_attributes_type_0 import EventDataPreviousAttributesType0  # noqa: PLC0415

        d = dict(src_dict)
        object_ = EventDataObject.from_dict(d.pop("object"))

        def _parse_previous_attributes(
            data: object,
        ) -> EventDataPreviousAttributesType0 | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                previous_attributes_type_0 = EventDataPreviousAttributesType0.from_dict(data)

                return previous_attributes_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(EventDataPreviousAttributesType0 | None | Unset, data)

        previous_attributes = _parse_previous_attributes(d.pop("previous_attributes", UNSET))

        event_data = cls(
            object_=object_,
            previous_attributes=previous_attributes,
        )

        event_data.additional_properties = d
        return event_data

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
