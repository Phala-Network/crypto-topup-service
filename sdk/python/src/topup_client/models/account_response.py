from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.api_key_object import ApiKeyObject
    from ..models.contact import Contact
    from ..models.due_diligence import DueDiligence


T = TypeVar("T", bound="AccountResponse")


@_attrs_define
class AccountResponse:
    """An account as the operator sees it.

    Attributes:
        api_keys (list[ApiKeyObject]): The secret keys this request issued, each with its `secret` shown only here: at
            creation
            a test key and, with `charges_enabled`, a live key; on an update that enables live mode,
            the first live key. Send them to the contact; the merchant rolls them on receipt.
        charges_enabled (bool): Whether the account may use live mode.
        contact (Contact): The merchant's contact recorded at onboarding (design D8): the operator's channel for the key
            hand-over, recovery, incidents, and restores, and the only personal data kept.
        created (int): Creation time, Unix seconds.
        due_diligence (DueDiligence): The record of the operator's offline due diligence (design D8): a reference to it,
            when, and
            by whom.
        id (str): Account id, `acct_…`.
        name (str): Display name.
        object_ (str): Always `account`.
        paused_scopes (list[str]): Active account-level pause scopes.
        restricted (bool): Whether the account is restricted for review.
    """

    api_keys: list[ApiKeyObject]
    charges_enabled: bool
    contact: Contact
    created: int
    due_diligence: DueDiligence
    id: str
    name: str
    object_: str
    paused_scopes: list[str]
    restricted: bool
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.api_key_object import ApiKeyObject  # noqa: PLC0415
        from ..models.contact import Contact  # noqa: PLC0415
        from ..models.due_diligence import DueDiligence  # noqa: PLC0415

        api_keys = []
        for api_keys_item_data in self.api_keys:
            api_keys_item = api_keys_item_data.to_dict()
            api_keys.append(api_keys_item)

        charges_enabled = self.charges_enabled

        contact = self.contact.to_dict()

        created = self.created

        due_diligence = self.due_diligence.to_dict()

        id = self.id

        name = self.name

        object_ = self.object_

        paused_scopes = self.paused_scopes

        restricted = self.restricted

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "api_keys": api_keys,
                "charges_enabled": charges_enabled,
                "contact": contact,
                "created": created,
                "due_diligence": due_diligence,
                "id": id,
                "name": name,
                "object": object_,
                "paused_scopes": paused_scopes,
                "restricted": restricted,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.api_key_object import ApiKeyObject  # noqa: PLC0415
        from ..models.contact import Contact  # noqa: PLC0415
        from ..models.due_diligence import DueDiligence  # noqa: PLC0415

        d = dict(src_dict)
        api_keys = []
        _api_keys = d.pop("api_keys")
        for api_keys_item_data in _api_keys:
            api_keys_item = ApiKeyObject.from_dict(api_keys_item_data)

            api_keys.append(api_keys_item)

        charges_enabled = d.pop("charges_enabled")

        contact = Contact.from_dict(d.pop("contact"))

        created = d.pop("created")

        due_diligence = DueDiligence.from_dict(d.pop("due_diligence"))

        id = d.pop("id")

        name = d.pop("name")

        object_ = d.pop("object")

        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        restricted = d.pop("restricted")

        account_response = cls(
            api_keys=api_keys,
            charges_enabled=charges_enabled,
            contact=contact,
            created=created,
            due_diligence=due_diligence,
            id=id,
            name=name,
            object_=object_,
            paused_scopes=paused_scopes,
            restricted=restricted,
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
