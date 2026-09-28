from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.config_asset import ConfigAsset


T = TypeVar("T", bound="Config")


@_attrs_define
class Config:
    """What a product's UI reads instead of hardcoding: assets, limits, and quote terms.

    Attributes:
        assets (list[ConfigAsset]): One entry per payable asset.
        currency (str): Credit currency, `usd`.
        max_open_amount_per_account (int): Per-account cap on the credit of open quotes, in cents; no single quote can
            exceed it.
        object_ (str): Always `config`.
        livemode (bool | Unset): The mode of the key that reads it: `assets` lists that mode's routes. Always sent;
            optional in the schema like the quote's.
    """

    assets: list[ConfigAsset]
    currency: str
    max_open_amount_per_account: int
    object_: str
    livemode: bool | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.config_asset import ConfigAsset  # noqa: PLC0415

        assets = []
        for assets_item_data in self.assets:
            assets_item = assets_item_data.to_dict()
            assets.append(assets_item)

        currency = self.currency

        max_open_amount_per_account = self.max_open_amount_per_account

        object_ = self.object_

        livemode = self.livemode

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "assets": assets,
                "currency": currency,
                "max_open_amount_per_account": max_open_amount_per_account,
                "object": object_,
            }
        )
        if livemode is not UNSET:
            field_dict["livemode"] = livemode

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.config_asset import ConfigAsset  # noqa: PLC0415

        d = dict(src_dict)
        assets = []
        _assets = d.pop("assets")
        for assets_item_data in _assets:
            assets_item = ConfigAsset.from_dict(assets_item_data)

            assets.append(assets_item)

        currency = d.pop("currency")

        max_open_amount_per_account = d.pop("max_open_amount_per_account")

        object_ = d.pop("object")

        livemode = d.pop("livemode", UNSET)

        config = cls(
            assets=assets,
            currency=currency,
            max_open_amount_per_account=max_open_amount_per_account,
            object_=object_,
            livemode=livemode,
        )

        config.additional_properties = d
        return config

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
