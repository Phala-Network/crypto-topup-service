from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast
from uuid import UUID
import datetime


T = TypeVar("T", bound="PendingDepositResponse")


@_attrs_define
class PendingDepositResponse:
    """A transfer to a persistent address seen above the finalized head. It is not a deposit, has not
    been credited, and may disappear in a reorg; once final it appears under `deposits`.

        Attributes:
            address (str): Receiving forwarder address.
            amount_atomic (str): Atomic token amount encoded as a decimal string.
            asset_contract (str): Canonical token contract address.
            block_number (int): Block that contains the transfer.
            block_time (datetime.datetime): Block time.
            chain_id (int): EVM chain identifier.
            confirmations (int): Blocks on top of and including that block at the last head scan.
            deposit_id (UUID): Identifier the deposit will have once final.
            estimated_final_at (datetime.datetime): Estimated finality time: block time plus 15 minutes.
            first_seen_at (datetime.datetime): First time the service saw the transfer.
            from_address (str): Canonical transfer sender address.
            log_index (int): Transfer log index.
            supported (bool): Whether a configured route accepts this token on this chain. Unsupported tokens are
                recorded but not credited once final.
            tx_hash (str): Canonical transaction hash.
    """

    address: str
    amount_atomic: str
    asset_contract: str
    block_number: int
    block_time: datetime.datetime
    chain_id: int
    confirmations: int
    deposit_id: UUID
    estimated_final_at: datetime.datetime
    first_seen_at: datetime.datetime
    from_address: str
    log_index: int
    supported: bool
    tx_hash: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        amount_atomic = self.amount_atomic

        asset_contract = self.asset_contract

        block_number = self.block_number

        block_time = self.block_time.isoformat()

        chain_id = self.chain_id

        confirmations = self.confirmations

        deposit_id = str(self.deposit_id)

        estimated_final_at = self.estimated_final_at.isoformat()

        first_seen_at = self.first_seen_at.isoformat()

        from_address = self.from_address

        log_index = self.log_index

        supported = self.supported

        tx_hash = self.tx_hash

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "amount_atomic": amount_atomic,
                "asset_contract": asset_contract,
                "block_number": block_number,
                "block_time": block_time,
                "chain_id": chain_id,
                "confirmations": confirmations,
                "deposit_id": deposit_id,
                "estimated_final_at": estimated_final_at,
                "first_seen_at": first_seen_at,
                "from_address": from_address,
                "log_index": log_index,
                "supported": supported,
                "tx_hash": tx_hash,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        amount_atomic = d.pop("amount_atomic")

        asset_contract = d.pop("asset_contract")

        block_number = d.pop("block_number")

        block_time = datetime.datetime.fromisoformat(d.pop("block_time"))

        chain_id = d.pop("chain_id")

        confirmations = d.pop("confirmations")

        deposit_id = UUID(d.pop("deposit_id"))

        estimated_final_at = datetime.datetime.fromisoformat(d.pop("estimated_final_at"))

        first_seen_at = datetime.datetime.fromisoformat(d.pop("first_seen_at"))

        from_address = d.pop("from_address")

        log_index = d.pop("log_index")

        supported = d.pop("supported")

        tx_hash = d.pop("tx_hash")

        pending_deposit_response = cls(
            address=address,
            amount_atomic=amount_atomic,
            asset_contract=asset_contract,
            block_number=block_number,
            block_time=block_time,
            chain_id=chain_id,
            confirmations=confirmations,
            deposit_id=deposit_id,
            estimated_final_at=estimated_final_at,
            first_seen_at=first_seen_at,
            from_address=from_address,
            log_index=log_index,
            supported=supported,
            tx_hash=tx_hash,
        )

        pending_deposit_response.additional_properties = d
        return pending_deposit_response

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
