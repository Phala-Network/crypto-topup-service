from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.api_key_object_object import ApiKeyObjectObject
from ..models.api_key_object_object import check_api_key_object_object
from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="ApiKeyObject")


@_attrs_define
class ApiKeyObject:
    """An API key (design D7). `secret` is present only in the response that created it.

    Example:
        {'created': 1790467200, 'expires_at': None, 'id': 'key_6a8c0e2b4d1f43a5c7e9b1d3f5a7c9e1', 'last_used':
            1790553600, 'livemode': False, 'name': 'fulfillment worker', 'object': 'api_key', 'permissions':
            ['account.read', 'deposit_addresses.read', 'deposit_addresses.write', 'deposits.read', 'events.read',
            'quotes.read', 'quotes.write', 'refunds.read'], 'redacted': 'ppay_rk_test_…Yz4x', 'status': 'active', 'type':
            'restricted'}

    Attributes:
        created (int): Creation time, Unix seconds.
        id (str): Key id, `key_…`.
        livemode (bool): The key's mode.
        name (str): The key's label.
        object_ (ApiKeyObjectObject): Always `api_key`.
        redacted (str): The key's prefix and last four characters, such as `ppay_sk_test_…a1B2`.
        status (str): `active`; `expiring` for a rolled key that still works until `expires_at`; `expired`;
            `revoked`.
        type_ (str): `secret`, holding every permission, or `restricted`, holding only `permissions`.
        expires_at (int | None | Unset): When a rolled key stops working, Unix seconds.
        last_used (int | None | Unset): Last use, Unix seconds, to the minute.
        permissions (list[str] | None | Unset): A restricted key's permissions, such as `quotes.write`; `null` for a
            secret key.
        secret (None | str | Unset): The whole key, `ppay_sk_…` or `ppay_rk_…`, shown once. Store it in a secret
            manager.
    """

    created: int
    id: str
    livemode: bool
    name: str
    object_: ApiKeyObjectObject
    redacted: str
    status: str
    type_: str
    expires_at: int | None | Unset = UNSET
    last_used: int | None | Unset = UNSET
    permissions: list[str] | None | Unset = UNSET
    secret: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        created = self.created

        id = self.id

        livemode = self.livemode

        name = self.name

        object_: str = self.object_

        redacted = self.redacted

        status = self.status

        type_ = self.type_

        expires_at: int | None | Unset
        if isinstance(self.expires_at, Unset):
            expires_at = UNSET
        else:
            expires_at = self.expires_at

        last_used: int | None | Unset
        if isinstance(self.last_used, Unset):
            last_used = UNSET
        else:
            last_used = self.last_used

        permissions: list[str] | None | Unset
        if isinstance(self.permissions, Unset):
            permissions = UNSET
        elif isinstance(self.permissions, list):
            permissions = self.permissions

        else:
            permissions = self.permissions

        secret: None | str | Unset
        if isinstance(self.secret, Unset):
            secret = UNSET
        else:
            secret = self.secret

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "created": created,
                "id": id,
                "livemode": livemode,
                "name": name,
                "object": object_,
                "redacted": redacted,
                "status": status,
                "type": type_,
            }
        )
        if expires_at is not UNSET:
            field_dict["expires_at"] = expires_at
        if last_used is not UNSET:
            field_dict["last_used"] = last_used
        if permissions is not UNSET:
            field_dict["permissions"] = permissions
        if secret is not UNSET:
            field_dict["secret"] = secret

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        created = d.pop("created")

        id = d.pop("id")

        livemode = d.pop("livemode")

        name = d.pop("name")

        object_ = check_api_key_object_object(d.pop("object"))

        redacted = d.pop("redacted")

        status = d.pop("status")

        type_ = d.pop("type")

        def _parse_expires_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        expires_at = _parse_expires_at(d.pop("expires_at", UNSET))

        def _parse_last_used(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        last_used = _parse_last_used(d.pop("last_used", UNSET))

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

        def _parse_secret(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        secret = _parse_secret(d.pop("secret", UNSET))

        api_key_object = cls(
            created=created,
            id=id,
            livemode=livemode,
            name=name,
            object_=object_,
            redacted=redacted,
            status=status,
            type_=type_,
            expires_at=expires_at,
            last_used=last_used,
            permissions=permissions,
            secret=secret,
        )

        api_key_object.additional_properties = d
        return api_key_object

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
