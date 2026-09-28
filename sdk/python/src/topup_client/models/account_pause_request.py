from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast


T = TypeVar("T", bound="AccountPauseRequest")


@_attrs_define
class AccountPauseRequest:
    """Administrative pause or resume of a whole account, in both modes.

    Attributes:
        reason (str): Why, for the audit log.
        scopes (list[str]): Pause scopes to add or remove: `quotes`, `settlement`, `refunds`.
    """

    reason: str
    scopes: list[str]

    def to_dict(self) -> dict[str, Any]:
        reason = self.reason

        scopes = self.scopes

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "reason": reason,
                "scopes": scopes,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        reason = d.pop("reason")

        scopes = cast(list[str], d.pop("scopes"))

        account_pause_request = cls(
            reason=reason,
            scopes=scopes,
        )

        return account_pause_request
