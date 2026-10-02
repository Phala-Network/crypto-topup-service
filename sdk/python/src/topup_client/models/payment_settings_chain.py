from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.payment_settings_asset import PaymentSettingsAsset


T = TypeVar("T", bound="PaymentSettingsChain")


@_attrs_define
class PaymentSettingsChain:
    """An accepted chain of your payment settings.

    Attributes:
        assets (list[PaymentSettingsAsset]): The accepted assets of the chain, at least one.
        chain_id (int): A chain of the key's mode.
        confirmations (None | str | Unset): The confirmation you require on the chain: a depth (`"12"`: the block and
            eleven more),
            `"safe"`, or `"finalized"`, never weaker than the chain's floor and of the chain's kind (a
            depth or `finalized` on Ethereum; a depth, `safe`, or `finalized` on an OP-stack chain).
            `null` for the floor.
    """

    assets: list[PaymentSettingsAsset]
    chain_id: int
    confirmations: None | str | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.payment_settings_asset import PaymentSettingsAsset  # noqa: PLC0415

        assets = []
        for assets_item_data in self.assets:
            assets_item = assets_item_data.to_dict()
            assets.append(assets_item)

        chain_id = self.chain_id

        confirmations: None | str | Unset
        if isinstance(self.confirmations, Unset):
            confirmations = UNSET
        else:
            confirmations = self.confirmations

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "assets": assets,
                "chain_id": chain_id,
            }
        )
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.payment_settings_asset import PaymentSettingsAsset  # noqa: PLC0415

        d = dict(src_dict)
        assets = []
        _assets = d.pop("assets")
        for assets_item_data in _assets:
            assets_item = PaymentSettingsAsset.from_dict(assets_item_data)

            assets.append(assets_item)

        chain_id = d.pop("chain_id")

        def _parse_confirmations(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        confirmations = _parse_confirmations(d.pop("confirmations", UNSET))

        payment_settings_chain = cls(
            assets=assets,
            chain_id=chain_id,
            confirmations=confirmations,
        )

        return payment_settings_chain
