from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset


T = TypeVar("T", bound="ConfigAsset")


@_attrs_define
class ConfigAsset:
    """A payable asset and its terms.

    Attributes:
        asset (str): Asset code.
        chain_id (int): EVM chain identifier.
        contract (str): Token contract address.
        decimals (int): Token decimals.
        max_deposit_atomic (str): Maximum creditable deposit in base units, as a decimal string.
        min_amount (int): Minimum credit in cents, for quotes and deposits; smaller deposits are not credited.
        min_refund_atomic (str): Minimum refundable amount in base units, as a decimal string.
        pricing (str): `spot` or `stablecoin`.
        quote_spread_bps (int): A quote's price is spot / (1 + spread_bps / 10 000); spot-valued payments carry no
            spread.
        quote_tolerance_bps (int): A payment within this many basis points of the quoted amount completes the quote.
        quote_ttl_seconds (int): Payment window of a quote, in seconds.
        typical_finality_seconds (int): Typical time from payment to finality, in seconds; refunds wait for it.
        confirmations (str | Unset): The confirmation a payment's block must reach before it is credited: a depth
            (`"2"`: the
            block and one more), `"safe"`, or `"finalized"`. A credit before finality can still be
            reversed (`deposit.reversed`). Optional in the schema, like `typical_credit_seconds`, so
            clients also parse responses from servers that predate fast credit.
        typical_credit_seconds (int | Unset): Typical time from payment to the `deposit.credited` event, in seconds.
    """

    asset: str
    chain_id: int
    contract: str
    decimals: int
    max_deposit_atomic: str
    min_amount: int
    min_refund_atomic: str
    pricing: str
    quote_spread_bps: int
    quote_tolerance_bps: int
    quote_ttl_seconds: int
    typical_finality_seconds: int
    confirmations: str | Unset = UNSET
    typical_credit_seconds: int | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        asset = self.asset

        chain_id = self.chain_id

        contract = self.contract

        decimals = self.decimals

        max_deposit_atomic = self.max_deposit_atomic

        min_amount = self.min_amount

        min_refund_atomic = self.min_refund_atomic

        pricing = self.pricing

        quote_spread_bps = self.quote_spread_bps

        quote_tolerance_bps = self.quote_tolerance_bps

        quote_ttl_seconds = self.quote_ttl_seconds

        typical_finality_seconds = self.typical_finality_seconds

        confirmations = self.confirmations

        typical_credit_seconds = self.typical_credit_seconds

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "asset": asset,
                "chain_id": chain_id,
                "contract": contract,
                "decimals": decimals,
                "max_deposit_atomic": max_deposit_atomic,
                "min_amount": min_amount,
                "min_refund_atomic": min_refund_atomic,
                "pricing": pricing,
                "quote_spread_bps": quote_spread_bps,
                "quote_tolerance_bps": quote_tolerance_bps,
                "quote_ttl_seconds": quote_ttl_seconds,
                "typical_finality_seconds": typical_finality_seconds,
            }
        )
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations
        if typical_credit_seconds is not UNSET:
            field_dict["typical_credit_seconds"] = typical_credit_seconds

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        contract = d.pop("contract")

        decimals = d.pop("decimals")

        max_deposit_atomic = d.pop("max_deposit_atomic")

        min_amount = d.pop("min_amount")

        min_refund_atomic = d.pop("min_refund_atomic")

        pricing = d.pop("pricing")

        quote_spread_bps = d.pop("quote_spread_bps")

        quote_tolerance_bps = d.pop("quote_tolerance_bps")

        quote_ttl_seconds = d.pop("quote_ttl_seconds")

        typical_finality_seconds = d.pop("typical_finality_seconds")

        confirmations = d.pop("confirmations", UNSET)

        typical_credit_seconds = d.pop("typical_credit_seconds", UNSET)

        config_asset = cls(
            asset=asset,
            chain_id=chain_id,
            contract=contract,
            decimals=decimals,
            max_deposit_atomic=max_deposit_atomic,
            min_amount=min_amount,
            min_refund_atomic=min_refund_atomic,
            pricing=pricing,
            quote_spread_bps=quote_spread_bps,
            quote_tolerance_bps=quote_tolerance_bps,
            quote_ttl_seconds=quote_ttl_seconds,
            typical_finality_seconds=typical_finality_seconds,
            confirmations=confirmations,
            typical_credit_seconds=typical_credit_seconds,
        )

        config_asset.additional_properties = d
        return config_asset

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
