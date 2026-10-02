from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.bounds_atomic import BoundsAtomic
    from ..models.bounds_u64 import BoundsU64


T = TypeVar("T", bound="AvailableAsset")


@_attrs_define
class AvailableAsset:
    """An asset of the operator's catalog and its bounds.

    Attributes:
        accepted (bool): Whether your settings list it.
        asset (str): Asset code.
        contract (str): Token contract address.
        decimals (int): Token decimals.
        enabled (bool): Whether its terms, each clamped to its bounds, can be used together; an accepted asset that
            is not enabled is not offered or credited until you or the operator change it.
        max_deposit_atomic (BoundsAtomic): The operator's default of a base-unit amount and its inclusive bounds, as
            decimal strings.
        min_amount (BoundsU64): The operator's default of an integer term and its inclusive bounds.
        min_deposit_atomic (BoundsAtomic): The operator's default of a base-unit amount and its inclusive bounds, as
            decimal strings.
        min_refund_atomic (BoundsAtomic): The operator's default of a base-unit amount and its inclusive bounds, as
            decimal strings.
        pricing (str): `spot` or `stablecoin`.
        quote_amount_decimals (int): Token decimals a quote's amount is rounded up to (the operator's).
        quote_spread_bps (BoundsU64): The operator's default of an integer term and its inclusive bounds.
        quote_tolerance_bps (BoundsU64): The operator's default of an integer term and its inclusive bounds.
        quote_ttl_seconds (BoundsU64): The operator's default of an integer term and its inclusive bounds.
    """

    accepted: bool
    asset: str
    contract: str
    decimals: int
    enabled: bool
    max_deposit_atomic: BoundsAtomic
    min_amount: BoundsU64
    min_deposit_atomic: BoundsAtomic
    min_refund_atomic: BoundsAtomic
    pricing: str
    quote_amount_decimals: int
    quote_spread_bps: BoundsU64
    quote_tolerance_bps: BoundsU64
    quote_ttl_seconds: BoundsU64
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.bounds_atomic import BoundsAtomic  # noqa: PLC0415
        from ..models.bounds_u64 import BoundsU64  # noqa: PLC0415

        accepted = self.accepted

        asset = self.asset

        contract = self.contract

        decimals = self.decimals

        enabled = self.enabled

        max_deposit_atomic = self.max_deposit_atomic.to_dict()

        min_amount = self.min_amount.to_dict()

        min_deposit_atomic = self.min_deposit_atomic.to_dict()

        min_refund_atomic = self.min_refund_atomic.to_dict()

        pricing = self.pricing

        quote_amount_decimals = self.quote_amount_decimals

        quote_spread_bps = self.quote_spread_bps.to_dict()

        quote_tolerance_bps = self.quote_tolerance_bps.to_dict()

        quote_ttl_seconds = self.quote_ttl_seconds.to_dict()

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "accepted": accepted,
                "asset": asset,
                "contract": contract,
                "decimals": decimals,
                "enabled": enabled,
                "max_deposit_atomic": max_deposit_atomic,
                "min_amount": min_amount,
                "min_deposit_atomic": min_deposit_atomic,
                "min_refund_atomic": min_refund_atomic,
                "pricing": pricing,
                "quote_amount_decimals": quote_amount_decimals,
                "quote_spread_bps": quote_spread_bps,
                "quote_tolerance_bps": quote_tolerance_bps,
                "quote_ttl_seconds": quote_ttl_seconds,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.bounds_atomic import BoundsAtomic  # noqa: PLC0415
        from ..models.bounds_u64 import BoundsU64  # noqa: PLC0415

        d = dict(src_dict)
        accepted = d.pop("accepted")

        asset = d.pop("asset")

        contract = d.pop("contract")

        decimals = d.pop("decimals")

        enabled = d.pop("enabled")

        max_deposit_atomic = BoundsAtomic.from_dict(d.pop("max_deposit_atomic"))

        min_amount = BoundsU64.from_dict(d.pop("min_amount"))

        min_deposit_atomic = BoundsAtomic.from_dict(d.pop("min_deposit_atomic"))

        min_refund_atomic = BoundsAtomic.from_dict(d.pop("min_refund_atomic"))

        pricing = d.pop("pricing")

        quote_amount_decimals = d.pop("quote_amount_decimals")

        quote_spread_bps = BoundsU64.from_dict(d.pop("quote_spread_bps"))

        quote_tolerance_bps = BoundsU64.from_dict(d.pop("quote_tolerance_bps"))

        quote_ttl_seconds = BoundsU64.from_dict(d.pop("quote_ttl_seconds"))

        available_asset = cls(
            accepted=accepted,
            asset=asset,
            contract=contract,
            decimals=decimals,
            enabled=enabled,
            max_deposit_atomic=max_deposit_atomic,
            min_amount=min_amount,
            min_deposit_atomic=min_deposit_atomic,
            min_refund_atomic=min_refund_atomic,
            pricing=pricing,
            quote_amount_decimals=quote_amount_decimals,
            quote_spread_bps=quote_spread_bps,
            quote_tolerance_bps=quote_tolerance_bps,
            quote_ttl_seconds=quote_ttl_seconds,
        )

        available_asset.additional_properties = d
        return available_asset

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
