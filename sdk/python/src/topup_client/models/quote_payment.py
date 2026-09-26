from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="QuotePayment")


@_attrs_define
class QuotePayment:
    """A payment observed at a quote's address. Display only: while `status` is `seen` it is not
    final, may still disappear in a reorg, and nothing has been credited.

        Attributes:
            amount_atomic (str): Token amount in base units, as a decimal string.
            deposit (str): `dep_` id the deposit has, or will have once final.
            matches_quote (bool): Whether the payment is the quote's asset, in time, and within tolerance, so it will be
                credited at the quoted price; otherwise it is credited at spot once final.
            status (str): `seen` (above the finalized head) or `final` (recorded as a deposit). New values may be
                added.
            tx_hash (str): Canonical transaction hash.
            confirmations (int | None | Unset): Blocks on top of and including the transfer's block at the last head scan;
                `seen` only.
            estimated_final_at (int | None | Unset): Estimated finality time, Unix seconds: block time plus 15 minutes;
                `seen` only.
    """

    amount_atomic: str
    deposit: str
    matches_quote: bool
    status: str
    tx_hash: str
    confirmations: int | None | Unset = UNSET
    estimated_final_at: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        amount_atomic = self.amount_atomic

        deposit = self.deposit

        matches_quote = self.matches_quote

        status = self.status

        tx_hash = self.tx_hash

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

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "deposit": deposit,
                "matches_quote": matches_quote,
                "status": status,
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

        deposit = d.pop("deposit")

        matches_quote = d.pop("matches_quote")

        status = d.pop("status")

        tx_hash = d.pop("tx_hash")

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

        quote_payment = cls(
            amount_atomic=amount_atomic,
            deposit=deposit,
            matches_quote=matches_quote,
            status=status,
            tx_hash=tx_hash,
            confirmations=confirmations,
            estimated_final_at=estimated_final_at,
        )

        quote_payment.additional_properties = d
        return quote_payment

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
