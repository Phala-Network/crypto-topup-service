from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="OperatorIdentity")


@_attrs_define
class OperatorIdentity:
    """The key a chain's flusher signs `flush` transactions with; it needs `OPERATOR_ROLE` and gas.

    Attributes:
        address (str): Operator address as lowercase `0x`-prefixed hexadecimal.
        chain_id (int): EVM chain identifier.
        keyid (str): Operator key identifier, `operator/v{operator_key_version}`.
        operator_key_version (int): The chain's configured operator key derivation version.
    """

    address: str
    chain_id: int
    keyid: str
    operator_key_version: int
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        chain_id = self.chain_id

        keyid = self.keyid

        operator_key_version = self.operator_key_version

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "chain_id": chain_id,
                "keyid": keyid,
                "operator_key_version": operator_key_version,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        chain_id = d.pop("chain_id")

        keyid = d.pop("keyid")

        operator_key_version = d.pop("operator_key_version")

        operator_identity = cls(
            address=address,
            chain_id=chain_id,
            keyid=keyid,
            operator_key_version=operator_key_version,
        )

        operator_identity.additional_properties = d
        return operator_identity

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
