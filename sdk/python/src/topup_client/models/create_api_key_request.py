from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="CreateApiKeyRequest")


@_attrs_define
class CreateApiKeyRequest:
    """`POST /v1/api_keys` body.

    Example:
        {'name': 'fulfillment worker', 'permissions': ['quotes.write', 'deposit_addresses.write', 'deposits.read',
            'events.read', 'refunds.read'], 'type': 'restricted'}

    Attributes:
        name (str | Unset): The key's label, at most 200 characters.
        permissions (list[str] | None | Unset): A restricted key's permissions, required with `type: restricted`: codes
            such as
            `quotes.write` or `deposits.read`, where a `write` includes its `read`. Grantable:
            `account.read`, `api_keys.read`, `quotes.*`, `deposit_addresses.*`, `deposits.*`,
            `refunds.*`, `events.read`, `endpoints.read`, `treasury.read`, `sweeps.read`,
            `forwarders.read`. Keys, treasuries, webhook endpoints, webhook keys, and account settings
            are managed only with a secret key.
        type_ (None | str | Unset): `secret`, the default, or `restricted` (Stripe's restricted keys): a key that holds
            only
            `permissions`.
    """

    name: str | Unset = UNSET
    permissions: list[str] | None | Unset = UNSET
    type_: None | str | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        name = self.name

        permissions: list[str] | None | Unset
        if isinstance(self.permissions, Unset):
            permissions = UNSET
        elif isinstance(self.permissions, list):
            permissions = self.permissions

        else:
            permissions = self.permissions

        type_: None | str | Unset
        if isinstance(self.type_, Unset):
            type_ = UNSET
        else:
            type_ = self.type_

        field_dict: dict[str, Any] = {}

        field_dict.update({})
        if name is not UNSET:
            field_dict["name"] = name
        if permissions is not UNSET:
            field_dict["permissions"] = permissions
        if type_ is not UNSET:
            field_dict["type"] = type_

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        name = d.pop("name", UNSET)

        def _parse_permissions(data: object) -> list[str] | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, list):
                    raise TypeError()
                permissions_type_0 = cast(list[str], data)

                return permissions_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(list[str] | None | Unset, data)

        permissions = _parse_permissions(d.pop("permissions", UNSET))

        def _parse_type_(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        type_ = _parse_type_(d.pop("type", UNSET))

        create_api_key_request = cls(
            name=name,
            permissions=permissions,
            type_=type_,
        )

        return create_api_key_request
