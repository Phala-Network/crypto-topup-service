from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="UpdateProductRequest")


@_attrs_define
class UpdateProductRequest:
    """Administrative replacement of an issued product's verification key and webhook URL. The key id
    stays the route's `destination.product_kid`.

        Attributes:
            public_key (str): Standard base64 of the product's new 32-byte ed25519 request-verification public key.
            reason (str): Why the credentials change, 1 to 1024 bytes: the rotation or incident it rests on.
            webhook_url (str): Absolute `https` URL of the product's webhook receiver; `http` only when the product's
                attested settlement URL also uses `http` (local stacks).
    """

    public_key: str
    reason: str
    webhook_url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        public_key = self.public_key

        reason = self.reason

        webhook_url = self.webhook_url

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "public_key": public_key,
                "reason": reason,
                "webhook_url": webhook_url,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        public_key = d.pop("public_key")

        reason = d.pop("reason")

        webhook_url = d.pop("webhook_url")

        update_product_request = cls(
            public_key=public_key,
            reason=reason,
            webhook_url=webhook_url,
        )

        update_product_request.additional_properties = d
        return update_product_request

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
