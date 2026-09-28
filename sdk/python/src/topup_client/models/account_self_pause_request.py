from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast


T = TypeVar("T", bound="AccountSelfPauseRequest")


@_attrs_define
class AccountSelfPauseRequest:
    """`POST /v1/account/pause` and `POST /v1/account/resume` body.

    Example:
        {'scopes': ['quotes']}

    Attributes:
        scopes (list[str]): `["quotes"]`, the one scope a merchant pauses itself: no quote, deposit address, or
            network is issued while it is paused. Existing addresses keep being credited.
    """

    scopes: list[str]

    def to_dict(self) -> dict[str, Any]:
        scopes = self.scopes

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "scopes": scopes,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        scopes = cast(list[str], d.pop("scopes"))

        account_self_pause_request = cls(
            scopes=scopes,
        )

        return account_self_pause_request
