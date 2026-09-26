from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast
import datetime


T = TypeVar("T", bound="ReconciliationBlockLiftResponse")


@_attrs_define
class ReconciliationBlockLiftResponse:
    """A lifted reconciliation block.

    Attributes:
        block_key (str): Lifted block, `chain:{chain_id}` or `address:{address_id}`.
        lifted_at (datetime.datetime): When the block was lifted; a repeated lift returns the original time.
    """

    block_key: str
    lifted_at: datetime.datetime
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        block_key = self.block_key

        lifted_at = self.lifted_at.isoformat()

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "block_key": block_key,
                "lifted_at": lifted_at,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        block_key = d.pop("block_key")

        lifted_at = datetime.datetime.fromisoformat(d.pop("lifted_at"))

        reconciliation_block_lift_response = cls(
            block_key=block_key,
            lifted_at=lifted_at,
        )

        reconciliation_block_lift_response.additional_properties = d
        return reconciliation_block_lift_response

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
