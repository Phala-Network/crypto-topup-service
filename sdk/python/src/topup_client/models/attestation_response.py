from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.webhook_key_object import WebhookKeyObject


T = TypeVar("T", bound="AttestationResponse")


@_attrs_define
class AttestationResponse:
    """TDX evidence binding a nonce to the webhook public keys of the caller's account in the
    caller's mode (design D11).

        Attributes:
            account (str): The caller's account, `acct_…`.
            livemode (bool): The caller's mode; each mode has its own key.
            object_ (str): Always `attestation`.
            report_data (str): `sha256(len(nonce) ‖ nonce ‖ len(account) ‖ account ‖ livemode ‖ (version ‖
                public_key)*)` as lowercase hexadecimal: lengths are one byte, `account` is UTF-8,
                `livemode` is one byte (`1` live, `0` test), and each key of `webhook_keys`, in order, is
                its version as 4 big-endian bytes and its 32 raw public-key bytes.
            tdx_quote (str): Versioned dstack TDX quote bytes as lowercase hexadecimal.
            webhook_keys (list[WebhookKeyObject]): The keys that sign the account's deliveries in this mode: the current one
                first, then any
                previous one still signing during a rotation.
    """

    account: str
    livemode: bool
    object_: str
    report_data: str
    tdx_quote: str
    webhook_keys: list[WebhookKeyObject]
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.webhook_key_object import WebhookKeyObject  # noqa: PLC0415

        account = self.account

        livemode = self.livemode

        object_ = self.object_

        report_data = self.report_data

        tdx_quote = self.tdx_quote

        webhook_keys = []
        for webhook_keys_item_data in self.webhook_keys:
            webhook_keys_item = webhook_keys_item_data.to_dict()
            webhook_keys.append(webhook_keys_item)

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "account": account,
                "livemode": livemode,
                "object": object_,
                "report_data": report_data,
                "tdx_quote": tdx_quote,
                "webhook_keys": webhook_keys,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.webhook_key_object import WebhookKeyObject  # noqa: PLC0415

        d = dict(src_dict)
        account = d.pop("account")

        livemode = d.pop("livemode")

        object_ = d.pop("object")

        report_data = d.pop("report_data")

        tdx_quote = d.pop("tdx_quote")

        webhook_keys = []
        _webhook_keys = d.pop("webhook_keys")
        for webhook_keys_item_data in _webhook_keys:
            webhook_keys_item = WebhookKeyObject.from_dict(webhook_keys_item_data)

            webhook_keys.append(webhook_keys_item)

        attestation_response = cls(
            account=account,
            livemode=livemode,
            object_=object_,
            report_data=report_data,
            tdx_quote=tdx_quote,
            webhook_keys=webhook_keys,
        )

        attestation_response.additional_properties = d
        return attestation_response

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
