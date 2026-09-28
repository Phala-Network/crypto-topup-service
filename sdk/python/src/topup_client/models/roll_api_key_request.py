from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset


T = TypeVar("T", bound="RollApiKeyRequest")


@_attrs_define
class RollApiKeyRequest:
    """`POST /v1/api_keys/{id}/roll` body.

    Attributes:
        expires_in (int | Unset): Seconds the old key keeps working, up to 604800 (7 days); 0, the default, revokes it
            at
            once.
    """

    expires_in: int | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        expires_in = self.expires_in

        field_dict: dict[str, Any] = {}

        field_dict.update({})
        if expires_in is not UNSET:
            field_dict["expires_in"] = expires_in

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        expires_in = d.pop("expires_in", UNSET)

        roll_api_key_request = cls(
            expires_in=expires_in,
        )

        return roll_api_key_request
