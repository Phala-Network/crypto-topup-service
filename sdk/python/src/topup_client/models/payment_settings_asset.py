from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="PaymentSettingsAsset")


@_attrs_define
class PaymentSettingsAsset:
    """An accepted asset and your terms on it; a term `null` or not sent takes the operator's default.

    Attributes:
        asset (str): An asset code routed on the chain, such as `usdc`.
        max_deposit_atomic (None | str | Unset): The maximum creditable deposit in base units, a decimal string.
        min_amount (int | None | Unset): The minimum credit in cents of a quote or a deposit valued at spot.
        min_deposit_atomic (None | str | Unset): The minimum creditable deposit in base units, a decimal string.
        min_refund_atomic (None | str | Unset): The refund dust floor in base units, a decimal string.
        quote_spread_bps (int | None | Unset): A quote's spread below spot, in basis points.
        quote_tolerance_bps (int | None | Unset): A quote's two-sided payment tolerance, in basis points.
        quote_ttl_seconds (int | None | Unset): A quote's payment window, in seconds.
    """

    asset: str
    max_deposit_atomic: None | str | Unset = UNSET
    min_amount: int | None | Unset = UNSET
    min_deposit_atomic: None | str | Unset = UNSET
    min_refund_atomic: None | str | Unset = UNSET
    quote_spread_bps: int | None | Unset = UNSET
    quote_tolerance_bps: int | None | Unset = UNSET
    quote_ttl_seconds: int | None | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        asset = self.asset

        max_deposit_atomic: None | str | Unset
        if isinstance(self.max_deposit_atomic, Unset):
            max_deposit_atomic = UNSET
        else:
            max_deposit_atomic = self.max_deposit_atomic

        min_amount: int | None | Unset
        if isinstance(self.min_amount, Unset):
            min_amount = UNSET
        else:
            min_amount = self.min_amount

        min_deposit_atomic: None | str | Unset
        if isinstance(self.min_deposit_atomic, Unset):
            min_deposit_atomic = UNSET
        else:
            min_deposit_atomic = self.min_deposit_atomic

        min_refund_atomic: None | str | Unset
        if isinstance(self.min_refund_atomic, Unset):
            min_refund_atomic = UNSET
        else:
            min_refund_atomic = self.min_refund_atomic

        quote_spread_bps: int | None | Unset
        if isinstance(self.quote_spread_bps, Unset):
            quote_spread_bps = UNSET
        else:
            quote_spread_bps = self.quote_spread_bps

        quote_tolerance_bps: int | None | Unset
        if isinstance(self.quote_tolerance_bps, Unset):
            quote_tolerance_bps = UNSET
        else:
            quote_tolerance_bps = self.quote_tolerance_bps

        quote_ttl_seconds: int | None | Unset
        if isinstance(self.quote_ttl_seconds, Unset):
            quote_ttl_seconds = UNSET
        else:
            quote_ttl_seconds = self.quote_ttl_seconds

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "asset": asset,
            }
        )
        if max_deposit_atomic is not UNSET:
            field_dict["max_deposit_atomic"] = max_deposit_atomic
        if min_amount is not UNSET:
            field_dict["min_amount"] = min_amount
        if min_deposit_atomic is not UNSET:
            field_dict["min_deposit_atomic"] = min_deposit_atomic
        if min_refund_atomic is not UNSET:
            field_dict["min_refund_atomic"] = min_refund_atomic
        if quote_spread_bps is not UNSET:
            field_dict["quote_spread_bps"] = quote_spread_bps
        if quote_tolerance_bps is not UNSET:
            field_dict["quote_tolerance_bps"] = quote_tolerance_bps
        if quote_ttl_seconds is not UNSET:
            field_dict["quote_ttl_seconds"] = quote_ttl_seconds

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        asset = d.pop("asset")

        def _parse_max_deposit_atomic(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        max_deposit_atomic = _parse_max_deposit_atomic(d.pop("max_deposit_atomic", UNSET))

        def _parse_min_amount(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        min_amount = _parse_min_amount(d.pop("min_amount", UNSET))

        def _parse_min_deposit_atomic(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        min_deposit_atomic = _parse_min_deposit_atomic(d.pop("min_deposit_atomic", UNSET))

        def _parse_min_refund_atomic(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        min_refund_atomic = _parse_min_refund_atomic(d.pop("min_refund_atomic", UNSET))

        def _parse_quote_spread_bps(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        quote_spread_bps = _parse_quote_spread_bps(d.pop("quote_spread_bps", UNSET))

        def _parse_quote_tolerance_bps(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        quote_tolerance_bps = _parse_quote_tolerance_bps(d.pop("quote_tolerance_bps", UNSET))

        def _parse_quote_ttl_seconds(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        quote_ttl_seconds = _parse_quote_ttl_seconds(d.pop("quote_ttl_seconds", UNSET))

        payment_settings_asset = cls(
            asset=asset,
            max_deposit_atomic=max_deposit_atomic,
            min_amount=min_amount,
            min_deposit_atomic=min_deposit_atomic,
            min_refund_atomic=min_refund_atomic,
            quote_spread_bps=quote_spread_bps,
            quote_tolerance_bps=quote_tolerance_bps,
            quote_ttl_seconds=quote_ttl_seconds,
        )

        return payment_settings_asset
