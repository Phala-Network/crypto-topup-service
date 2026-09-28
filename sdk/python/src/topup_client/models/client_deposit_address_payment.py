from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="ClientDepositAddressPayment")


@_attrs_define
class ClientDepositAddressPayment:
    """A payment to a deposit address, as its public view shows it.

    Attributes:
        amount_atomic (str): Token amount in base units, as a decimal string.
        chain_id (int): EVM chain identifier.
        created (int): When the payment was first seen or recorded, Unix seconds.
        status (str): `seen` (in a block, below the route's confirmation, and may still disappear);
            `confirming` (at the confirmation, being valued and screened); `credited`; `rejected`
            (not credited; the payer should contact the merchant's support); or `reversed` (its
            transaction left the chain before finality: the payment did not happen).
        tx_hash (str): Transaction hash.
        asset (None | str | Unset): Asset code; `null` for a token without a route.
        confirmations (int | None | Unset): While `seen`: blocks on top of and including the payment's block; otherwise
            `null`.
        decimals (int | None | Unset): The token's decimals, to display `amount_atomic`; `null` with `asset`.
    """

    amount_atomic: str
    chain_id: int
    created: int
    status: str
    tx_hash: str
    asset: None | str | Unset = UNSET
    confirmations: int | None | Unset = UNSET
    decimals: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        amount_atomic = self.amount_atomic

        chain_id = self.chain_id

        created = self.created

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

        decimals: int | None | Unset
        if isinstance(self.decimals, Unset):
            decimals = UNSET
        else:
            decimals = self.decimals

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "chain_id": chain_id,
                "created": created,
                "status": status,
                "tx_hash": tx_hash,
            }
        )
        if asset is not UNSET:
            field_dict["asset"] = asset
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations
        if decimals is not UNSET:
            field_dict["decimals"] = decimals

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        amount_atomic = d.pop("amount_atomic")

        chain_id = d.pop("chain_id")

        created = d.pop("created")

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

        def _parse_decimals(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        decimals = _parse_decimals(d.pop("decimals", UNSET))

        client_deposit_address_payment = cls(
            amount_atomic=amount_atomic,
            chain_id=chain_id,
            created=created,
            status=status,
            tx_hash=tx_hash,
            asset=asset,
            confirmations=confirmations,
            decimals=decimals,
        )

        client_deposit_address_payment.additional_properties = d
        return client_deposit_address_payment

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
