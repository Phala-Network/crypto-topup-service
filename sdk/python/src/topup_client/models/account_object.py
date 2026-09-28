from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.confirmation_policy import ConfirmationPolicy
    from ..models.webhook_key_version import WebhookKeyVersion


T = TypeVar("T", bound="AccountObject")


@_attrs_define
class AccountObject:
    """The account of the request's API key (`GET /v1/account`), in the key's mode.

    Attributes:
        charges_enabled (bool): Whether the operator enabled live mode.
        confirmation_policies (list[ConfirmationPolicy]): The confirmations you require on chains of this mode, stricter
            than the routes' (design
            D1); a chain not listed uses its route's (`GET /v1/config`).
        created (int): Creation time, Unix seconds.
        id (str): Account id, `acct_…`.
        livemode (bool): The mode of the key that reads it.
        name (str): Display name.
        object_ (str): Always `account`.
        paused_scopes (list[str]): Active account-level pause scopes, the operator's and your own (`POST
            /v1/account/pause`):
            while `quotes` is listed, no quote, deposit address, or network is issued. Your resume
            lifts only your own pause.
        webhook_keys (list[WebhookKeyVersion]): The keys that sign this mode's webhooks: the current one first, then any
            previous one
            still signing during a rotation. Their public keys come from `GET /v1/attestation`.
    """

    charges_enabled: bool
    confirmation_policies: list[ConfirmationPolicy]
    created: int
    id: str
    livemode: bool
    name: str
    object_: str
    paused_scopes: list[str]
    webhook_keys: list[WebhookKeyVersion]
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.confirmation_policy import ConfirmationPolicy  # noqa: PLC0415
        from ..models.webhook_key_version import WebhookKeyVersion  # noqa: PLC0415

        charges_enabled = self.charges_enabled

        confirmation_policies = []
        for confirmation_policies_item_data in self.confirmation_policies:
            confirmation_policies_item = confirmation_policies_item_data.to_dict()
            confirmation_policies.append(confirmation_policies_item)

        created = self.created

        id = self.id

        livemode = self.livemode

        name = self.name

        object_ = self.object_

        paused_scopes = self.paused_scopes

        webhook_keys = []
        for webhook_keys_item_data in self.webhook_keys:
            webhook_keys_item = webhook_keys_item_data.to_dict()
            webhook_keys.append(webhook_keys_item)

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "charges_enabled": charges_enabled,
                "confirmation_policies": confirmation_policies,
                "created": created,
                "id": id,
                "livemode": livemode,
                "name": name,
                "object": object_,
                "paused_scopes": paused_scopes,
                "webhook_keys": webhook_keys,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.confirmation_policy import ConfirmationPolicy  # noqa: PLC0415
        from ..models.webhook_key_version import WebhookKeyVersion  # noqa: PLC0415

        d = dict(src_dict)
        charges_enabled = d.pop("charges_enabled")

        confirmation_policies = []
        _confirmation_policies = d.pop("confirmation_policies")
        for confirmation_policies_item_data in _confirmation_policies:
            confirmation_policies_item = ConfirmationPolicy.from_dict(
                confirmation_policies_item_data
            )

            confirmation_policies.append(confirmation_policies_item)

        created = d.pop("created")

        id = d.pop("id")

        livemode = d.pop("livemode")

        name = d.pop("name")

        object_ = d.pop("object")

        paused_scopes = cast(list[str], d.pop("paused_scopes"))

        webhook_keys = []
        _webhook_keys = d.pop("webhook_keys")
        for webhook_keys_item_data in _webhook_keys:
            webhook_keys_item = WebhookKeyVersion.from_dict(webhook_keys_item_data)

            webhook_keys.append(webhook_keys_item)

        account_object = cls(
            charges_enabled=charges_enabled,
            confirmation_policies=confirmation_policies,
            created=created,
            id=id,
            livemode=livemode,
            name=name,
            object_=object_,
            paused_scopes=paused_scopes,
            webhook_keys=webhook_keys,
        )

        account_object.additional_properties = d
        return account_object

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
