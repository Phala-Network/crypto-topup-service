from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="PersistentSaltInputs")


@_attrs_define
class PersistentSaltInputs:
    """Inputs needed to recompute a persistent CREATE2 address.

    Attributes:
        external_id (str): Product-owned account identifier.
        product_slug (str): Stable product slug.
        version (int): Persistent address version.
    """

    external_id: str
    product_slug: str
    version: int
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        external_id = self.external_id

        product_slug = self.product_slug

        version = self.version

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "external_id": external_id,
                "product_slug": product_slug,
                "version": version,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        external_id = d.pop("external_id")

        product_slug = d.pop("product_slug")

        version = d.pop("version")

        persistent_salt_inputs = cls(
            external_id=external_id,
            product_slug=product_slug,
            version=version,
        )

        persistent_salt_inputs.additional_properties = d
        return persistent_salt_inputs

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
