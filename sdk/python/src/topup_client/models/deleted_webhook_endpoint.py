from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="DeletedWebhookEndpoint")


@_attrs_define
class DeletedWebhookEndpoint:
    """A deleted webhook endpoint.

    Attributes:
        deleted (bool): Always `true`.
        id (str): Endpoint id, `we_…`.
        object_ (str): Always `webhook_endpoint`.
    """

    deleted: bool
    id: str
    object_: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        deleted = self.deleted

        id = self.id

        object_ = self.object_

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "deleted": deleted,
                "id": id,
                "object": object_,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        deleted = d.pop("deleted")

        id = d.pop("id")

        object_ = d.pop("object")

        deleted_webhook_endpoint = cls(
            deleted=deleted,
            id=id,
            object_=object_,
        )

        deleted_webhook_endpoint.additional_properties = d
        return deleted_webhook_endpoint

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
