from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.persistent_salt_inputs import PersistentSaltInputs


T = TypeVar("T", bound="DepositAddressResponse")


@_attrs_define
class DepositAddressResponse:
    """Persistent deposit address and deterministic derivation inputs.

    Attributes:
        address (str): Canonical EVM address.
        chain_id (int): EVM chain identifier.
        route (str): Route name governing deposits to this address.
        salt (str): Canonical CREATE2 salt.
        salt_inputs (PersistentSaltInputs): Inputs needed to recompute a persistent CREATE2 address.
    """

    address: str
    chain_id: int
    route: str
    salt: str
    salt_inputs: PersistentSaltInputs
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.persistent_salt_inputs import PersistentSaltInputs  # noqa: PLC0415

        address = self.address

        chain_id = self.chain_id

        route = self.route

        salt = self.salt

        salt_inputs = self.salt_inputs.to_dict()

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "chain_id": chain_id,
                "route": route,
                "salt": salt,
                "salt_inputs": salt_inputs,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.persistent_salt_inputs import PersistentSaltInputs  # noqa: PLC0415

        d = dict(src_dict)
        address = d.pop("address")

        chain_id = d.pop("chain_id")

        route = d.pop("route")

        salt = d.pop("salt")

        salt_inputs = PersistentSaltInputs.from_dict(d.pop("salt_inputs"))

        deposit_address_response = cls(
            address=address,
            chain_id=chain_id,
            route=route,
            salt=salt,
            salt_inputs=salt_inputs,
        )

        deposit_address_response.additional_properties = d
        return deposit_address_response

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
