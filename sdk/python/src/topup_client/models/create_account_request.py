from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="CreateAccountRequest")


@_attrs_define
class CreateAccountRequest:
    """Administrative account issuance body, until self-serve signup (design PR 5) and API keys
    (design PR 6) replace it.

        Attributes:
            livemode (bool): The mode the account's signing key acts in: `true` for live routes, `false` for test
                routes.
            name (str): Display name, 1 to 200 characters.
            public_key (str): Standard base64 of the account's 32-byte ed25519 request-verification public key.
            webhook_url (str): Absolute `https` URL of the account's webhook receiver; `http` only when the service's
                own public origin uses `http` (local stacks).
    """

    livemode: bool
    name: str
    public_key: str
    webhook_url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        livemode = self.livemode

        name = self.name

        public_key = self.public_key

        webhook_url = self.webhook_url

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "livemode": livemode,
                "name": name,
                "public_key": public_key,
                "webhook_url": webhook_url,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        livemode = d.pop("livemode")

        name = d.pop("name")

        public_key = d.pop("public_key")

        webhook_url = d.pop("webhook_url")

        create_account_request = cls(
            livemode=livemode,
            name=name,
            public_key=public_key,
            webhook_url=webhook_url,
        )

        create_account_request.additional_properties = d
        return create_account_request

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
