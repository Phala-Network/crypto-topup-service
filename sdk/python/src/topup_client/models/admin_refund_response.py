from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="AdminRefundResponse")


@_attrs_define
class AdminRefundResponse:
    """Administrative refund workflow result.

    Attributes:
        id (str): Refund id, `re_…`.
        status (str): Stable workflow status.
        confirmation_evidence (Any | Unset): Most recent confirmation evidence, when checked.
        tx_hash (None | str | Unset): Recorded treasury transaction hash, when present.
    """

    id: str
    status: str
    confirmation_evidence: Any | Unset = UNSET
    tx_hash: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        id = self.id

        status = self.status

        confirmation_evidence = self.confirmation_evidence

        tx_hash: None | str | Unset
        if isinstance(self.tx_hash, Unset):
            tx_hash = UNSET
        else:
            tx_hash = self.tx_hash

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "id": id,
                "status": status,
            }
        )
        if confirmation_evidence is not UNSET:
            field_dict["confirmation_evidence"] = confirmation_evidence
        if tx_hash is not UNSET:
            field_dict["tx_hash"] = tx_hash

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        id = d.pop("id")

        status = d.pop("status")

        confirmation_evidence = d.pop("confirmation_evidence", UNSET)

        def _parse_tx_hash(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        tx_hash = _parse_tx_hash(d.pop("tx_hash", UNSET))

        admin_refund_response = cls(
            id=id,
            status=status,
            confirmation_evidence=confirmation_evidence,
            tx_hash=tx_hash,
        )

        admin_refund_response.additional_properties = d
        return admin_refund_response

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
