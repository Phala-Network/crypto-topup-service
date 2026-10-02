from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.deposit_address_asset import DepositAddressAsset


T = TypeVar("T", bound="ClientDepositAddressNetwork")


@_attrs_define
class ClientDepositAddressNetwork:
    """A deposit address on one network, as its public view shows it.

    Attributes:
        address (str): The forwarder address to pay on this chain.
        assets (list[DepositAddressAsset]): The supported tokens on this chain.
        chain_id (int): EVM chain identifier.
        typical_credit_seconds (int | Unset): Typical time from payment to credit, in seconds, at the confirmation this
            chain's payments
            are credited at, as `GET /v1/config` reports it for each of the chain's tokens, which share
            the chain's one floor. Absent from a service of v0.3.5 or earlier, so a client should not
            require it.
    """

    address: str
    assets: list[DepositAddressAsset]
    chain_id: int
    typical_credit_seconds: int | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit_address_asset import DepositAddressAsset  # noqa: PLC0415

        address = self.address

        assets = []
        for assets_item_data in self.assets:
            assets_item = assets_item_data.to_dict()
            assets.append(assets_item)

        chain_id = self.chain_id

        typical_credit_seconds = self.typical_credit_seconds

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "assets": assets,
                "chain_id": chain_id,
            }
        )
        if typical_credit_seconds is not UNSET:
            field_dict["typical_credit_seconds"] = typical_credit_seconds

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit_address_asset import DepositAddressAsset  # noqa: PLC0415

        d = dict(src_dict)
        address = d.pop("address")

        assets = []
        _assets = d.pop("assets")
        for assets_item_data in _assets:
            assets_item = DepositAddressAsset.from_dict(assets_item_data)

            assets.append(assets_item)

        chain_id = d.pop("chain_id")

        typical_credit_seconds = d.pop("typical_credit_seconds", UNSET)

        client_deposit_address_network = cls(
            address=address,
            assets=assets,
            chain_id=chain_id,
            typical_credit_seconds=typical_credit_seconds,
        )

        client_deposit_address_network.additional_properties = d
        return client_deposit_address_network

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
