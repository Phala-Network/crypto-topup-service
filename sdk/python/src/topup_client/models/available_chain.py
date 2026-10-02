from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.available_asset import AvailableAsset
    from ..models.available_confirmations import AvailableConfirmations


T = TypeVar("T", bound="AvailableChain")


@_attrs_define
class AvailableChain:
    """A chain of the operator's catalog in the key's mode.

    Attributes:
        assets (list[AvailableAsset]): The chain's assets.
        chain_id (int): EVM chain identifier.
        confirmations (AvailableConfirmations): A chain's confirmation floor.
        status (str): `active` (accepted, with a treasury), `treasury_not_set` (accepted, without a treasury:
            nothing is offered on it until you set one), or `not_configured`.
    """

    assets: list[AvailableAsset]
    chain_id: int
    confirmations: AvailableConfirmations
    status: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.available_asset import AvailableAsset  # noqa: PLC0415
        from ..models.available_confirmations import AvailableConfirmations  # noqa: PLC0415

        assets = []
        for assets_item_data in self.assets:
            assets_item = assets_item_data.to_dict()
            assets.append(assets_item)

        chain_id = self.chain_id

        confirmations = self.confirmations.to_dict()

        status = self.status

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "assets": assets,
                "chain_id": chain_id,
                "confirmations": confirmations,
                "status": status,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.available_asset import AvailableAsset  # noqa: PLC0415
        from ..models.available_confirmations import AvailableConfirmations  # noqa: PLC0415

        d = dict(src_dict)
        assets = []
        _assets = d.pop("assets")
        for assets_item_data in _assets:
            assets_item = AvailableAsset.from_dict(assets_item_data)

            assets.append(assets_item)

        chain_id = d.pop("chain_id")

        confirmations = AvailableConfirmations.from_dict(d.pop("confirmations"))

        status = d.pop("status")

        available_chain = cls(
            assets=assets,
            chain_id=chain_id,
            confirmations=confirmations,
            status=status,
        )

        available_chain.additional_properties = d
        return available_chain

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
