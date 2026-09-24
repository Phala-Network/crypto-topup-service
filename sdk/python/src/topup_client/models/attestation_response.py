from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.operator_identity import OperatorIdentity


T = TypeVar("T", bound="AttestationResponse")


@_attrs_define
class AttestationResponse:
    """TDX evidence binding a nonce to the settlement public key and the flusher operators.

    Attributes:
        keyid (str): Settlement key identifier.
        quote (str): Versioned dstack attestation bytes as lowercase hexadecimal.
        report_data (str): `sha256(nonce ‖ settlement_pubkey ‖ record_1 ‖ … ‖ record_n)` as lowercase hexadecimal,
            with one 32-byte record per operator in list order: `chain_id` (u64 big-endian),
            `operator_key_version` (u32 big-endian), and the 20 address bytes.
        settlement_pubkey (str): Raw ed25519 settlement public key as lowercase hexadecimal.
        operators (list[OperatorIdentity] | Unset): Flusher operator of each configured chain, in ascending `chain_id`
            order. This service
            always sends it; it is optional in the schema so clients also parse responses from servers
            that predate it, whose report data binds no operators (an absent list reads as empty).
    """

    keyid: str
    quote: str
    report_data: str
    settlement_pubkey: str
    operators: list[OperatorIdentity] | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.operator_identity import OperatorIdentity  # noqa: PLC0415

        keyid = self.keyid

        quote = self.quote

        report_data = self.report_data

        settlement_pubkey = self.settlement_pubkey

        operators: list[dict[str, Any]] | Unset = UNSET
        if not isinstance(self.operators, Unset):
            operators = []
            for operators_item_data in self.operators:
                operators_item = operators_item_data.to_dict()
                operators.append(operators_item)

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
        if operators is not UNSET:
            field_dict["operators"] = operators

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.operator_identity import OperatorIdentity  # noqa: PLC0415

        d = dict(src_dict)
        keyid = d.pop("keyid")

        quote = d.pop("quote")

        report_data = d.pop("report_data")

        settlement_pubkey = d.pop("settlement_pubkey")

        _operators = d.pop("operators", UNSET)
        operators: list[OperatorIdentity] | Unset = UNSET
        if _operators is not UNSET:
            operators = []
            for operators_item_data in _operators:
                operators_item = OperatorIdentity.from_dict(operators_item_data)

                operators.append(operators_item)

        attestation_response = cls(
            keyid=keyid,
            quote=quote,
            report_data=report_data,
            settlement_pubkey=settlement_pubkey,
            operators=operators,
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
