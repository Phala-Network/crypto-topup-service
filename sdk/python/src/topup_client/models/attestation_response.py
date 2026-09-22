from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="AttestationResponse")


@_attrs_define
class AttestationResponse:
    """TDX evidence binding a nonce to the settlement public key.

    Attributes:
        keyid (str): Settlement key identifier.
        quote (str): Versioned dstack attestation bytes as lowercase hexadecimal.
        report_data (str): SHA-256 report data as lowercase hexadecimal.
        settlement_pubkey (str): Raw ed25519 settlement public key as lowercase hexadecimal.
    """

    keyid: str
    quote: str
    report_data: str
    settlement_pubkey: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        keyid = self.keyid

        quote = self.quote

        report_data = self.report_data

        settlement_pubkey = self.settlement_pubkey

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "keyid": keyid,
                "quote": quote,
                "report_data": report_data,
                "settlement_pubkey": settlement_pubkey,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        keyid = d.pop("keyid")

        quote = d.pop("quote")

        report_data = d.pop("report_data")

        settlement_pubkey = d.pop("settlement_pubkey")

        attestation_response = cls(
            keyid=keyid,
            quote=quote,
            report_data=report_data,
            settlement_pubkey=settlement_pubkey,
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
