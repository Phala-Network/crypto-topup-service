from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast
from uuid import UUID
import datetime


T = TypeVar("T", bound="ReconciliationBlockReport")


@_attrs_define
class ReconciliationBlockReport:
    """A persistent reconciliation block (architecture §13).

    Attributes:
        block_key (str): Block identifier, `chain:{chain_id}`.
        chain_id (int): EVM chain identifier.
        check (str): Check that wrote the block, such as `address_derivation`.
        created_at (datetime.datetime): When the block was written.
        reason (str): Why the check blocked.
        scope (str): `chain`: the chain is frozen. No check writes the `address` scope any more; it excluded an
            address from the removed operator flusher.
        address_id (None | Unset | UUID): Blocked address for an `address` block.
    """

    block_key: str
    chain_id: int
    check: str
    created_at: datetime.datetime
    reason: str
    scope: str
    address_id: None | Unset | UUID = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        block_key = self.block_key

        chain_id = self.chain_id

        check = self.check

        created_at = self.created_at.isoformat()

        reason = self.reason

        scope = self.scope

        address_id: None | str | Unset
        if isinstance(self.address_id, Unset):
            address_id = UNSET
        elif isinstance(self.address_id, UUID):
            address_id = str(self.address_id)
        else:
            address_id = self.address_id

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "block_key": block_key,
                "chain_id": chain_id,
                "check": check,
                "created_at": created_at,
                "reason": reason,
                "scope": scope,
            }
        )
        if address_id is not UNSET:
            field_dict["address_id"] = address_id

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        block_key = d.pop("block_key")

        chain_id = d.pop("chain_id")

        check = d.pop("check")

        created_at = datetime.datetime.fromisoformat(d.pop("created_at"))

        reason = d.pop("reason")

        scope = d.pop("scope")

        def _parse_address_id(data: object) -> None | Unset | UUID:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, str):
                    raise TypeError()
                address_id_type_0 = UUID(data)

                return address_id_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(None | Unset | UUID, data)

        address_id = _parse_address_id(d.pop("address_id", UNSET))

        reconciliation_block_report = cls(
            block_key=block_key,
            chain_id=chain_id,
            check=check,
            created_at=created_at,
            reason=reason,
            scope=scope,
            address_id=address_id,
        )

        reconciliation_block_report.additional_properties = d
        return reconciliation_block_report

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
