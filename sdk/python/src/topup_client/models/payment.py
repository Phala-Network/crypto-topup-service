from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="Payment")


@_attrs_define
class Payment:
    """A payment observed at a quote's address or a deposit address. Display only: while `status` is
    `seen` it may still disappear in a reorg, and nothing has been credited.

        Attributes:
            amount_atomic (str): Token amount in base units, as a decimal string.
            chain_id (int): EVM chain identifier.
            deposit (str): `dep_` id the deposit has, or will have once recorded.
            status (str): `seen` (in a block, not recorded as a deposit yet) or `recorded` (recorded as a deposit at
                the route's confirmation; follow it as `deposit`). New values may be added.
            tx_hash (str): Canonical transaction hash.
            asset (None | str | Unset): Asset code; `null` for a token without a route.
            confirmations (int | None | Unset): Blocks on top of and including the transfer's block at the last head scan;
                `seen` only.
            estimated_final_at (int | None | Unset): Estimated finality time, Unix seconds: block time plus 15 minutes;
                `seen` only.
            matches_quote (bool | None | Unset): On a quote: whether the payment is the quote's asset, in time, and within
                tolerance, so
                it is credited at the quoted price (otherwise at spot). `null` on a deposit address, whose
                payments are all credited at spot.
    """

    amount_atomic: str
    chain_id: int
    deposit: str
    status: str
    tx_hash: str
    asset: None | str | Unset = UNSET
    confirmations: int | None | Unset = UNSET
    estimated_final_at: int | None | Unset = UNSET
    matches_quote: bool | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        amount_atomic = self.amount_atomic

        chain_id = self.chain_id

        deposit = self.deposit

        status = self.status

        tx_hash = self.tx_hash

        asset: None | str | Unset
        if isinstance(self.asset, Unset):
            asset = UNSET
        else:
            asset = self.asset

        confirmations: int | None | Unset
        if isinstance(self.confirmations, Unset):
            confirmations = UNSET
        else:
            confirmations = self.confirmations

        estimated_final_at: int | None | Unset
        if isinstance(self.estimated_final_at, Unset):
            estimated_final_at = UNSET
        else:
            estimated_final_at = self.estimated_final_at

        matches_quote: bool | None | Unset
        if isinstance(self.matches_quote, Unset):
            matches_quote = UNSET
        else:
            matches_quote = self.matches_quote

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "chain_id": chain_id,
                "deposit": deposit,
                "status": status,
                "tx_hash": tx_hash,
            }
        )
        if asset is not UNSET:
            field_dict["asset"] = asset
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations
        if estimated_final_at is not UNSET:
            field_dict["estimated_final_at"] = estimated_final_at
        if matches_quote is not UNSET:
            field_dict["matches_quote"] = matches_quote

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        amount_atomic = d.pop("amount_atomic")

        chain_id = d.pop("chain_id")

        deposit = d.pop("deposit")

        status = d.pop("status")

        tx_hash = d.pop("tx_hash")

        def _parse_asset(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        asset = _parse_asset(d.pop("asset", UNSET))

        def _parse_confirmations(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        confirmations = _parse_confirmations(d.pop("confirmations", UNSET))

        def _parse_estimated_final_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        estimated_final_at = _parse_estimated_final_at(d.pop("estimated_final_at", UNSET))

        def _parse_matches_quote(data: object) -> bool | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(bool | None | Unset, data)

        matches_quote = _parse_matches_quote(d.pop("matches_quote", UNSET))

        payment = cls(
            amount_atomic=amount_atomic,
            chain_id=chain_id,
            deposit=deposit,
            status=status,
            tx_hash=tx_hash,
            asset=asset,
            confirmations=confirmations,
            estimated_final_at=estimated_final_at,
            matches_quote=matches_quote,
        )

        payment.additional_properties = d
        return payment

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
