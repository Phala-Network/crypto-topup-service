from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast


T = TypeVar("T", bound="AccountResponse")


@_attrs_define
class AccountResponse:
    """An issued account and its request signing credential.

    Attributes:
        id (str): Account id, `acct_…`.
        key_id (str): The key id the account signs its requests with, `{id}/v1`.
        livemode (bool): The mode the account's signing key acts in.
        name (str): Display name.
        paused_scopes (list[str]): Active account-level pause scopes.
        public_key (str): Standard base64 of the account's ed25519 public key.
        webhook_url (str): Webhook receiver URL.
    """

    id: str
    key_id: str
    livemode: bool
    name: str
    paused_scopes: list[str]
    public_key: str
    webhook_url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        id = self.id

        key_id = self.key_id

        livemode = self.livemode

        name = self.name

        paused_scopes = self.paused_scopes

        public_key = self.public_key

        webhook_url = self.webhook_url

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "id": id,
                "key_id": key_id,
                "livemode": livemode,
                "name": name,
                "paused_scopes": paused_scopes,
                "public_key": public_key,
                "webhook_url": webhook_url,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        id = d.pop("id")

        key_id = d.pop("key_id")

        livemode = d.pop("livemode")

        name = d.pop("name")

        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        public_key = d.pop("public_key")

        webhook_url = d.pop("webhook_url")

        account_response = cls(
            id=id,
            key_id=key_id,
            livemode=livemode,
            name=name,
            paused_scopes=paused_scopes,
            public_key=public_key,
            webhook_url=webhook_url,
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
