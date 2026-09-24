from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast
from uuid import UUID


T = TypeVar("T", bound="ProductResponse")


@_attrs_define
class ProductResponse:
    """Registered product.

    Attributes:
        id (UUID): Service product identifier.
        paused_scopes (list[str]): Active product-level pause scopes.
        public_key (str): Standard base64 of the product's ed25519 public key.
        slug (str): Product slug.
        webhook_url (str): Webhook receiver URL.
    """

    id: UUID
    paused_scopes: list[str]
    public_key: str
    slug: str
    webhook_url: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        id = str(self.id)

        paused_scopes = self.paused_scopes

        public_key = self.public_key

        slug = self.slug

        webhook_url = self.webhook_url

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "id": id,
                "paused_scopes": paused_scopes,
                "public_key": public_key,
                "slug": slug,
                "webhook_url": webhook_url,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        id = UUID(d.pop("id"))

        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        public_key = d.pop("public_key")

        slug = d.pop("slug")

        webhook_url = d.pop("webhook_url")

        product_response = cls(
            id=id,
            paused_scopes=paused_scopes,
            public_key=public_key,
            slug=slug,
            webhook_url=webhook_url,
        )

        product_response.additional_properties = d
        return product_response

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
