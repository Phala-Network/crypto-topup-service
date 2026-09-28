from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="Contact")


@_attrs_define
class Contact:
    """The merchant's contact recorded at onboarding (design D8): the operator's channel for the key
    hand-over, recovery, incidents, and restores, and the only personal data kept.

        Attributes:
            email (str): The security contact's email address.
            name (str): The contact's name, 1 to 200 characters.
    """

    email: str
    name: str

    def to_dict(self) -> dict[str, Any]:
        email = self.email

        name = self.name

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "email": email,
                "name": name,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        email = d.pop("email")

        name = d.pop("name")

        contact = cls(
            email=email,
            name=name,
        )

        return contact
