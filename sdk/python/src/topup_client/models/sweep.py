from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="Sweep")


@_attrs_define
class Sweep:
    """A sweep: one finalized `Flushed` event of the factory, which moved a forwarder's whole balance
    of a token to its treasury (Stripe's Payout). Anyone can send the `flush`; the merchant usually
    does, with the SDK's `flush_transaction` or `safe_batch`.

        Attributes:
            address (str): The forwarder's address.
            amount_atomic (str): Amount moved, in base units, as a decimal string.
            block_number (int): Block number of the transaction.
            chain_id (int): EVM chain identifier.
            created (int): When the finalized event was indexed, Unix seconds.
            forwarder (str): The forwarder swept, `fwd_…`.
            id (str): Sweep id, `sw_…`.
            livemode (bool): The mode.
            log_index (int): Block-wide index of the `Flushed` log.
            object_ (str): Always `sweep`.
            token (str): The token contract.
            treasury (str): The treasury paid, fixed in the forwarder's address.
            tx_hash (str): The flush transaction.
            asset (None | str | Unset): Asset code; `null` for a token without a route.
    """

    address: str
    amount_atomic: str
    block_number: int
    chain_id: int
    created: int
    forwarder: str
    id: str
    livemode: bool
    log_index: int
    object_: str
    token: str
    treasury: str
    tx_hash: str
    asset: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        amount_atomic = self.amount_atomic

        block_number = self.block_number

        chain_id = self.chain_id

        created = self.created

        forwarder = self.forwarder

        id = self.id

        livemode = self.livemode

        log_index = self.log_index

        object_ = self.object_

        token = self.token

        treasury = self.treasury

        tx_hash = self.tx_hash

        asset: None | str | Unset
        if isinstance(self.asset, Unset):
            asset = UNSET
        else:
            asset = self.asset

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "amount_atomic": amount_atomic,
                "block_number": block_number,
                "chain_id": chain_id,
                "created": created,
                "forwarder": forwarder,
                "id": id,
                "livemode": livemode,
                "log_index": log_index,
                "object": object_,
                "token": token,
                "treasury": treasury,
                "tx_hash": tx_hash,
            }
        )
        if asset is not UNSET:
            field_dict["asset"] = asset

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        amount_atomic = d.pop("amount_atomic")

        block_number = d.pop("block_number")

        chain_id = d.pop("chain_id")

        created = d.pop("created")

        forwarder = d.pop("forwarder")

        id = d.pop("id")

        livemode = d.pop("livemode")

        log_index = d.pop("log_index")

        object_ = d.pop("object")

        token = d.pop("token")

        treasury = d.pop("treasury")

        tx_hash = d.pop("tx_hash")

        def _parse_asset(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        asset = _parse_asset(d.pop("asset", UNSET))

        sweep = cls(
            address=address,
            amount_atomic=amount_atomic,
            block_number=block_number,
            chain_id=chain_id,
            created=created,
            forwarder=forwarder,
            id=id,
            livemode=livemode,
            log_index=log_index,
            object_=object_,
            token=token,
            treasury=treasury,
            tx_hash=tx_hash,
            asset=asset,
        )

        sweep.additional_properties = d
        return sweep

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
