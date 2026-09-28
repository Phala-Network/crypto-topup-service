from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.contact import Contact


T = TypeVar("T", bound="UpdateAccountRequest")


@_attrs_define
class UpdateAccountRequest:
    """`POST /v1/admin/accounts/{account}` body; absent fields stay as they are.

    Attributes:
        reason (str): Why, 1 to 1024 bytes.
        charges_enabled (bool | None | Unset): Enables or disables live mode. Enabling it for an account without a live
            key returns the
            account's first live key.
        contact (Contact | None | Unset):
        restricted (bool | None | Unset): Marks the account restricted for review.
        webhook_url (None | str | Unset): Replaces the URL of the account's webhook endpoints (until design PR 8).
    """

    reason: str
    charges_enabled: bool | None | Unset = UNSET
    contact: Contact | None | Unset = UNSET
    restricted: bool | None | Unset = UNSET
    webhook_url: None | str | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.contact import Contact  # noqa: PLC0415

        reason = self.reason

        charges_enabled: bool | None | Unset
        if isinstance(self.charges_enabled, Unset):
            charges_enabled = UNSET
        else:
            charges_enabled = self.charges_enabled

        contact: dict[str, Any] | None | Unset
        if isinstance(self.contact, Unset):
            contact = UNSET
        elif isinstance(self.contact, Contact):
            contact = self.contact.to_dict()
        else:
            contact = self.contact

        restricted: bool | None | Unset
        if isinstance(self.restricted, Unset):
            restricted = UNSET
        else:
            restricted = self.restricted

        webhook_url: None | str | Unset
        if isinstance(self.webhook_url, Unset):
            webhook_url = UNSET
        else:
            webhook_url = self.webhook_url

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "reason": reason,
            }
        )
        if charges_enabled is not UNSET:
            field_dict["charges_enabled"] = charges_enabled
        if contact is not UNSET:
            field_dict["contact"] = contact
        if restricted is not UNSET:
            field_dict["restricted"] = restricted
        if webhook_url is not UNSET:
            field_dict["webhook_url"] = webhook_url

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.contact import Contact  # noqa: PLC0415

        d = dict(src_dict)
        reason = d.pop("reason")

        def _parse_charges_enabled(data: object) -> bool | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(bool | None | Unset, data)

        charges_enabled = _parse_charges_enabled(d.pop("charges_enabled", UNSET))

        def _parse_contact(data: object) -> Contact | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                contact_type_0 = Contact.from_dict(data)

                return contact_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(Contact | None | Unset, data)

        contact = _parse_contact(d.pop("contact", UNSET))

        def _parse_restricted(data: object) -> bool | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(bool | None | Unset, data)

        restricted = _parse_restricted(d.pop("restricted", UNSET))

        def _parse_webhook_url(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        webhook_url = _parse_webhook_url(d.pop("webhook_url", UNSET))

        update_account_request = cls(
            reason=reason,
            charges_enabled=charges_enabled,
            contact=contact,
            restricted=restricted,
            webhook_url=webhook_url,
        )

        return update_account_request
