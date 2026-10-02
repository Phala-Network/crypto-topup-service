from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="BoundsU64")


@_attrs_define
class BoundsU64:
    """The operator's default of an integer term and its inclusive bounds.

    Attributes:
        default (int): The value when you set none.
        max_ (int): The highest value you may set.
        min_ (int): The lowest value you may set.
    """

    default: int
    max_: int
    min_: int
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        default = self.default

        max_ = self.max_

        min_ = self.min_

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "default": default,
                "max": max_,
                "min": min_,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        default = d.pop("default")

        max_ = d.pop("max")

        min_ = d.pop("min")

        bounds_u64 = cls(
            default=default,
            max_=max_,
            min_=min_,
        )

        bounds_u64.additional_properties = d
        return bounds_u64

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
