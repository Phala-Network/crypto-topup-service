from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="CreateQuoteRequest")


@_attrs_define
class CreateQuoteRequest:
    """`POST /v1/quotes` body.

    Attributes:
        account_id (str): Your identifier of the customer account to credit, 1 to 200 characters; the account is
            created on its first quote.
        amount (int): The credit to quote, a positive integer in the currency's minor unit (US cents).
        asset (str): Asset code of the payment on that chain, such as `pha`.
        chain_id (int): EVM chain of the payment, one of `GET /v1/config` `assets[].chain_id`.
        currency (str): Lowercase ISO currency code; only `usd`.
    """

    account_id: str
    amount: int
    asset: str
    chain_id: int
    currency: str

    def to_dict(self) -> dict[str, Any]:
        account_id = self.account_id

        amount = self.amount

        asset = self.asset

        chain_id = self.chain_id

        currency = self.currency

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "account_id": account_id,
                "amount": amount,
                "asset": asset,
                "chain_id": chain_id,
                "currency": currency,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        account_id = d.pop("account_id")

        amount = d.pop("amount")

        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        currency = d.pop("currency")

        create_quote_request = cls(
            account_id=account_id,
            amount=amount,
            asset=asset,
            chain_id=chain_id,
            currency=currency,
        )

        return create_quote_request
