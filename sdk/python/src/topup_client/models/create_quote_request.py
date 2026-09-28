from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.metadata_clear import MetadataClear
from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.metadata_param_type_0 import MetadataParamType0


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
        metadata (MetadataClear | MetadataParamType0 | Unset): A `metadata` parameter: an object of string values, where
            `""` unsets the key, or `""` to
            unset every key.
    """

    account_id: str
    amount: int
    asset: str
    chain_id: int
    currency: str
    metadata: MetadataClear | MetadataParamType0 | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        account_id = self.account_id

        amount = self.amount

        asset = self.asset

        chain_id = self.chain_id

        currency = self.currency

        metadata: dict[str, Any] | str | Unset
        if isinstance(self.metadata, Unset):
            metadata = UNSET
        elif isinstance(self.metadata, MetadataParamType0):
            metadata = self.metadata.to_dict()
        else:
            metadata = self.metadata.value

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
        if metadata is not UNSET:
            field_dict["metadata"] = metadata

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        d = dict(src_dict)
        account_id = d.pop("account_id")

        amount = d.pop("amount")

        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        currency = d.pop("currency")

        def _parse_metadata(data: object) -> MetadataClear | MetadataParamType0 | Unset:
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_metadata_param_type_0 = MetadataParamType0.from_dict(data)

                return componentsschemas_metadata_param_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            if not isinstance(data, str):
                raise TypeError()
            componentsschemas_metadata_param_type_1 = MetadataClear(data)

            return componentsschemas_metadata_param_type_1

        metadata = _parse_metadata(d.pop("metadata", UNSET))

        create_quote_request = cls(
            account_id=account_id,
            amount=amount,
            asset=asset,
            chain_id=chain_id,
            currency=currency,
            metadata=metadata,
        )

        return create_quote_request
