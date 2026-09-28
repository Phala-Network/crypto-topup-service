from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="BalanceAmount")


@_attrs_define
class BalanceAmount:
    """A token's unswept amounts on one chain.

    Attributes:
        amount_atomic (str): Every deposit not reversed, minus finalized sweeps, in base units, as a decimal string.
        chain_id (int): EVM chain identifier.
        final_amount_atomic (str): The part of `amount_atomic` from final deposits, which can no longer be reversed:
            what is
            safe to sweep.
        token (str): The token contract.
        asset (None | str | Unset): Asset code; `null` for a token without a route.
    """

    amount_atomic: str
    chain_id: int
    final_amount_atomic: str
    token: str
    asset: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        amount_atomic = self.amount_atomic

        chain_id = self.chain_id

        final_amount_atomic = self.final_amount_atomic

        token = self.token

        asset: None | str | Unset
        if isinstance(self.asset, Unset):
            asset = UNSET
        else:
            asset = self.asset

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "chain_id": chain_id,
                "final_amount_atomic": final_amount_atomic,
                "token": token,
            }
        )
        if asset is not UNSET:
            field_dict["asset"] = asset

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        amount_atomic = d.pop("amount_atomic")

        chain_id = d.pop("chain_id")

        final_amount_atomic = d.pop("final_amount_atomic")

        token = d.pop("token")

        def _parse_asset(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        asset = _parse_asset(d.pop("asset", UNSET))

        balance_amount = cls(
            amount_atomic=amount_atomic,
            chain_id=chain_id,
            final_amount_atomic=final_amount_atomic,
            token=token,
            asset=asset,
        )

        balance_amount.additional_properties = d
        return balance_amount

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
