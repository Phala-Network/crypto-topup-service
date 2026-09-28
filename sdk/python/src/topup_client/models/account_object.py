from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast


T = TypeVar("T", bound="AccountObject")


@_attrs_define
class AccountObject:
    """The account of the request's API key (`GET /v1/account`), in the key's mode.

    Attributes:
        charges_enabled (bool): Whether the operator enabled live mode.
        created (int): Creation time, Unix seconds.
        id (str): Account id, `acct_…`.
        livemode (bool): The mode of the key that reads it.
        name (str): Display name.
        object_ (str): Always `account`.
        paused_scopes (list[str]): Active account-level pause scopes.
    """

    charges_enabled: bool
    created: int
    id: str
    livemode: bool
    name: str
    object_: str
    paused_scopes: list[str]
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        charges_enabled = self.charges_enabled

        created = self.created

        id = self.id

        livemode = self.livemode

        name = self.name

        object_ = self.object_

        paused_scopes = self.paused_scopes

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "charges_enabled": charges_enabled,
                "created": created,
                "id": id,
                "livemode": livemode,
                "name": name,
                "object": object_,
                "paused_scopes": paused_scopes,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        charges_enabled = d.pop("charges_enabled")

        created = d.pop("created")

        id = d.pop("id")

        livemode = d.pop("livemode")

        name = d.pop("name")

        object_ = d.pop("object")

        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        account_object = cls(
            charges_enabled=charges_enabled,
            created=created,
            id=id,
            livemode=livemode,
            name=name,
            object_=object_,
            paused_scopes=paused_scopes,
        )

        account_object.additional_properties = d
        return account_object

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
