from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="QuoteTerms")


@_attrs_define
class QuoteTerms:
    """The terms a quote was issued with.

    Attributes:
        confirmations (str): The confirmation the quote required when issued: a depth, `safe`, or `finalized`. A payment
            not credited yet waits for the stricter of it and the chain's current floor.
        max_deposit_atomic (str): Maximum creditable deposit in base units, as a decimal string.
        min_amount (int): Minimum credit in cents of a payment valued at spot.
        min_deposit_atomic (str): Minimum creditable deposit in base units, as a decimal string.
        min_refund_atomic (str): Refund dust floor in base units, as a decimal string.
        quote_amount_decimals (int): Token decimals `amount_atomic` was rounded up to.
        quote_spread_bps (int): The spread below spot of the locked price, in basis points.
        quote_tolerance_bps (int): A payment within this many basis points of `amount_atomic`, either way, completes the
            quote at its `amount`.
        quote_ttl_seconds (int): The payment window, in seconds.
    """

    confirmations: str
    max_deposit_atomic: str
    min_amount: int
    min_deposit_atomic: str
    min_refund_atomic: str
    quote_amount_decimals: int
    quote_spread_bps: int
    quote_tolerance_bps: int
    quote_ttl_seconds: int
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        confirmations = self.confirmations

        max_deposit_atomic = self.max_deposit_atomic

        min_amount = self.min_amount

        min_deposit_atomic = self.min_deposit_atomic

        min_refund_atomic = self.min_refund_atomic

        quote_amount_decimals = self.quote_amount_decimals

        quote_spread_bps = self.quote_spread_bps

        quote_tolerance_bps = self.quote_tolerance_bps

        quote_ttl_seconds = self.quote_ttl_seconds

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "confirmations": confirmations,
                "max_deposit_atomic": max_deposit_atomic,
                "min_amount": min_amount,
                "min_deposit_atomic": min_deposit_atomic,
                "min_refund_atomic": min_refund_atomic,
                "quote_amount_decimals": quote_amount_decimals,
                "quote_spread_bps": quote_spread_bps,
                "quote_tolerance_bps": quote_tolerance_bps,
                "quote_ttl_seconds": quote_ttl_seconds,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        confirmations = d.pop("confirmations")

        max_deposit_atomic = d.pop("max_deposit_atomic")

        min_amount = d.pop("min_amount")

        min_deposit_atomic = d.pop("min_deposit_atomic")

        min_refund_atomic = d.pop("min_refund_atomic")

        quote_amount_decimals = d.pop("quote_amount_decimals")

        quote_spread_bps = d.pop("quote_spread_bps")

        quote_tolerance_bps = d.pop("quote_tolerance_bps")

        quote_ttl_seconds = d.pop("quote_ttl_seconds")

        quote_terms = cls(
            confirmations=confirmations,
            max_deposit_atomic=max_deposit_atomic,
            min_amount=min_amount,
            min_deposit_atomic=min_deposit_atomic,
            min_refund_atomic=min_refund_atomic,
            quote_amount_decimals=quote_amount_decimals,
            quote_spread_bps=quote_spread_bps,
            quote_tolerance_bps=quote_tolerance_bps,
            quote_ttl_seconds=quote_ttl_seconds,
        )

        quote_terms.additional_properties = d
        return quote_terms

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
