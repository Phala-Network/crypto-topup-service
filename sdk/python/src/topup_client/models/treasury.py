from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="Treasury")


@_attrs_define
class Treasury:
    """An account's treasury of one chain and mode (design D10): the only address the forwarders
    issued over it can pay.

        Attributes:
            address (str): The treasury address.
            chain_id (int): EVM chain identifier.
            created (int): When it was proven, Unix seconds.
            effective_at (int): When the treasury applies or applied, Unix seconds: at once for a chain's first treasury
                and in test mode, 48 hours after the proof for a later live change.
            id (str): Treasury id, `trs_…`.
            kind (str): `eoa` (an EIP-191 signature recovered to the address) or `contract` (a deployed
                contract's EIP-1271 approval).
            livemode (bool): The mode.
            object_ (str): Always `treasury`.
            status (str): `pending` (a live change waiting for `effective_at`; cancel it with
                `POST /v1/treasuries/{id}/cancel`), `active` (the chain's current treasury: new quotes and
                deposit address networks pay it), `replaced` (a former treasury; addresses issued over it
                still pay it), or `canceled`.
            canceled_at (int | None | Unset): When it was canceled, Unix seconds.
            cancellation_reason (None | str | Unset): Why it was canceled: `requested` (you canceled it) or `sanctioned` (a
                sanctions list named
                the address when the change was due to apply, so it never applied).
            replaced_at (int | None | Unset): When a later treasury replaced it, Unix seconds.
    """

    address: str
    chain_id: int
    created: int
    effective_at: int
    id: str
    kind: str
    livemode: bool
    object_: str
    status: str
    canceled_at: int | None | Unset = UNSET
    cancellation_reason: None | str | Unset = UNSET
    replaced_at: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        chain_id = self.chain_id

        created = self.created

        effective_at = self.effective_at

        id = self.id

        kind = self.kind

        livemode = self.livemode

        object_ = self.object_

        status = self.status

        canceled_at: int | None | Unset
        if isinstance(self.canceled_at, Unset):
            canceled_at = UNSET
        else:
            canceled_at = self.canceled_at

        cancellation_reason: None | str | Unset
        if isinstance(self.cancellation_reason, Unset):
            cancellation_reason = UNSET
        else:
            cancellation_reason = self.cancellation_reason

        replaced_at: int | None | Unset
        if isinstance(self.replaced_at, Unset):
            replaced_at = UNSET
        else:
            replaced_at = self.replaced_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "chain_id": chain_id,
                "created": created,
                "effective_at": effective_at,
                "id": id,
                "kind": kind,
                "livemode": livemode,
                "object": object_,
                "status": status,
            }
        )
        if canceled_at is not UNSET:
            field_dict["canceled_at"] = canceled_at
        if cancellation_reason is not UNSET:
            field_dict["cancellation_reason"] = cancellation_reason
        if replaced_at is not UNSET:
            field_dict["replaced_at"] = replaced_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        chain_id = d.pop("chain_id")

        created = d.pop("created")

        effective_at = d.pop("effective_at")

        id = d.pop("id")

        kind = d.pop("kind")

        livemode = d.pop("livemode")

        object_ = d.pop("object")

        status = d.pop("status")

        def _parse_canceled_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        canceled_at = _parse_canceled_at(d.pop("canceled_at", UNSET))

        def _parse_cancellation_reason(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        cancellation_reason = _parse_cancellation_reason(d.pop("cancellation_reason", UNSET))

        def _parse_replaced_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        replaced_at = _parse_replaced_at(d.pop("replaced_at", UNSET))

        treasury = cls(
            address=address,
            chain_id=chain_id,
            created=created,
            effective_at=effective_at,
            id=id,
            kind=kind,
            livemode=livemode,
            object_=object_,
            status=status,
            canceled_at=canceled_at,
            cancellation_reason=cancellation_reason,
            replaced_at=replaced_at,
        )

        treasury.additional_properties = d
        return treasury

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
