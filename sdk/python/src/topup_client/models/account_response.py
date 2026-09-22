from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast
from uuid import UUID


T = TypeVar("T", bound="AccountResponse")


@_attrs_define
class AccountResponse:
    """Product-owned account.

    Attributes:
        external_id (str): Product-owned account identifier.
        id (UUID): Service account identifier.
        paused_scopes (list[str]): Active account-level pause scopes.
        status (str): Workspace lifecycle state.
    """

    external_id: str
    id: UUID
    paused_scopes: list[str]
    status: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        external_id = self.external_id

        id = str(self.id)

        paused_scopes = self.paused_scopes

        status = self.status

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "external_id": external_id,
                "id": id,
                "paused_scopes": paused_scopes,
                "status": status,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        external_id = d.pop("external_id")

        id = UUID(d.pop("id"))

        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        status = d.pop("status")

        account_response = cls(
            external_id=external_id,
            id=id,
            paused_scopes=paused_scopes,
            status=status,
        )

        account_response.additional_properties = d
        return account_response

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
