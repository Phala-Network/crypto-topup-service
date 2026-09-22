from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast


T = TypeVar("T", bound="RoutePauseResponse")


@_attrs_define
class RoutePauseResponse:
    """Administrative route pause response.

    Attributes:
        paused_scopes (list[str]): Current route-level pause scopes.
        route (str): Route name.
    """

    paused_scopes: list[str]
    route: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        paused_scopes = self.paused_scopes

        route = self.route

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "paused_scopes": paused_scopes,
                "route": route,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        route = d.pop("route")

        route_pause_response = cls(
            paused_scopes=paused_scopes,
            route=route,
        )

        route_pause_response.additional_properties = d
        return route_pause_response

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
