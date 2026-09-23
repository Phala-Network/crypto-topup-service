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


T = TypeVar("T", bound="RateLockPayment")


@_attrs_define
class RateLockPayment:
    """The first payment observed at a rate-lock address.

    Attributes:
        amount_atomic (str): Atomic token amount encoded as a decimal string.
        amount_within_tolerance (bool): Whether the amount is the lock's asset within the lock tolerance.
        asset_contract (str): Canonical token contract address.
        block_number (int): Block that contains the transfer.
        deposit_id (UUID): Identifier the deposit has, or will have once final.
        in_time (bool): Whether the block time is at or before `expires_at`.
        log_index (int): Transfer log index.
        status (str): `seen`: in a block above `finalized`, provisional and may still disappear in a reorg.
            `finalized`: recorded as a deposit; follow it by `deposit_id`. New values may be added.
        supported (bool): Whether the token is the lock's route asset.
        tx_hash (str): Canonical transaction hash.
        confirmations (int | None | Unset): Blocks on top of and including that block at the last head scan; `seen`
            only.
        estimated_final_at (datetime.datetime | None | Unset): Estimated finality time: block time plus 15 minutes;
            `seen` only.
    """

    amount_atomic: str
    amount_within_tolerance: bool
    asset_contract: str
    block_number: int
    deposit_id: UUID
    in_time: bool
    log_index: int
    status: str
    supported: bool
    tx_hash: str
    confirmations: int | None | Unset = UNSET
    estimated_final_at: datetime.datetime | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        amount_atomic = self.amount_atomic

        amount_within_tolerance = self.amount_within_tolerance

        asset_contract = self.asset_contract

        block_number = self.block_number

        deposit_id = str(self.deposit_id)

        in_time = self.in_time

        log_index = self.log_index

        status = self.status

        supported = self.supported

        tx_hash = self.tx_hash

        confirmations: int | None | Unset
        if isinstance(self.confirmations, Unset):
            confirmations = UNSET
        else:
            confirmations = self.confirmations

        estimated_final_at: None | str | Unset
        if isinstance(self.estimated_final_at, Unset):
            estimated_final_at = UNSET
        elif isinstance(self.estimated_final_at, datetime.datetime):
            estimated_final_at = self.estimated_final_at.isoformat()
        else:
            estimated_final_at = self.estimated_final_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "amount_within_tolerance": amount_within_tolerance,
                "asset_contract": asset_contract,
                "block_number": block_number,
                "deposit_id": deposit_id,
                "in_time": in_time,
                "log_index": log_index,
                "status": status,
                "supported": supported,
                "tx_hash": tx_hash,
            }
        )
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations
        if estimated_final_at is not UNSET:
            field_dict["estimated_final_at"] = estimated_final_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        amount_atomic = d.pop("amount_atomic")

        amount_within_tolerance = d.pop("amount_within_tolerance")

        asset_contract = d.pop("asset_contract")

        block_number = d.pop("block_number")

        deposit_id = UUID(d.pop("deposit_id"))

        in_time = d.pop("in_time")

        log_index = d.pop("log_index")

        status = d.pop("status")

        supported = d.pop("supported")

        tx_hash = d.pop("tx_hash")

        def _parse_confirmations(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        confirmations = _parse_confirmations(d.pop("confirmations", UNSET))

        def _parse_estimated_final_at(data: object) -> datetime.datetime | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, str):
                    raise TypeError()
                estimated_final_at_type_0 = datetime.datetime.fromisoformat(data)

                return estimated_final_at_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(datetime.datetime | None | Unset, data)

        estimated_final_at = _parse_estimated_final_at(d.pop("estimated_final_at", UNSET))

        rate_lock_payment = cls(
            amount_atomic=amount_atomic,
            amount_within_tolerance=amount_within_tolerance,
            asset_contract=asset_contract,
            block_number=block_number,
            deposit_id=deposit_id,
            in_time=in_time,
            log_index=log_index,
            status=status,
            supported=supported,
            tx_hash=tx_hash,
            confirmations=confirmations,
            estimated_final_at=estimated_final_at,
        )

        rate_lock_payment.additional_properties = d
        return rate_lock_payment

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
