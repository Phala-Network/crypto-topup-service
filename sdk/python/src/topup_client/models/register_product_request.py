from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="RegisterProductRequest")


@_attrs_define
class RegisterProductRequest:
    """Administrative product registration body. The product's key id is not part of it: the
    attested route is its only source.

        Attributes:
            public_key (str): Standard base64 of the product's 32-byte ed25519 request-verification public key.
            slug (str): Product slug named by a loaded route's `destination.product`; matches
                `^[a-z0-9][a-z0-9-]{0,62}$`.
            webhook_url (str): Absolute `https` URL of the product's webhook receiver; `http` only when the service's
                own public origin uses `http` (local stacks).
    """

    public_key: str
    slug: str
    webhook_url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        public_key = self.public_key

        slug = self.slug

        webhook_url = self.webhook_url

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "public_key": public_key,
                "slug": slug,
                "webhook_url": webhook_url,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        public_key = d.pop("public_key")

        slug = d.pop("slug")

        webhook_url = d.pop("webhook_url")

        register_product_request = cls(
            public_key=public_key,
            slug=slug,
            webhook_url=webhook_url,
        )

        register_product_request.additional_properties = d
        return register_product_request

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
