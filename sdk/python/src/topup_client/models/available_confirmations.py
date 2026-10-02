from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="AvailableConfirmations")


@_attrs_define
class AvailableConfirmations:
    """A chain's confirmation floor.

    Attributes:
        default (str): The confirmation of a chain whose `confirmations` is `null`: the floor.
        floor (str): The weakest confirmation you may require, a depth, `safe`, or `finalized`.
    """

    default: str
    floor: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        default = self.default

        floor = self.floor

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "default": default,
                "floor": floor,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        default = d.pop("default")

        floor = d.pop("floor")

        available_confirmations = cls(
            default=default,
            floor=floor,
        )

        available_confirmations.additional_properties = d
        return available_confirmations

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
