from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="Forwarder")


@_attrs_define
class Forwarder:
    """A forwarder the account was issued (design §13): everything needed to recompute its address
    and sweep it without Phala Pay. The address is `factory`'s `CREATE2` clone of the pinned
    implementation over `treasury` and `salt`.

        Attributes:
            address (str): The forwarder address.
            chain_id (int): EVM chain identifier.
            factory (str): The forwarder factory.
            id (str): Forwarder id, `fwd_…`.
            livemode (bool): The mode.
            object_ (str): Always `forwarder`.
            salt (str): The `CREATE2` salt, 32 bytes of hex.
            treasury (str): The treasury the forwarder pays, fixed in its address.
            deposit_address (None | str | Unset): The deposit address it is a network of, `da_…`; `null` for a quote's.
            quote (None | str | Unset): The quote it was issued for, `qt_…`; `null` for a deposit address's network.
            superseded_at (int | None | Unset): When a treasury change replaced this deposit address network, Unix seconds;
                it is still
                watched and credited, and pays its own treasury.
    """

    address: str
    chain_id: int
    factory: str
    id: str
    livemode: bool
    object_: str
    salt: str
    treasury: str
    deposit_address: None | str | Unset = UNSET
    quote: None | str | Unset = UNSET
    superseded_at: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        chain_id = self.chain_id

        factory = self.factory

        id = self.id

        livemode = self.livemode

        object_ = self.object_

        salt = self.salt

        treasury = self.treasury

        deposit_address: None | str | Unset
        if isinstance(self.deposit_address, Unset):
            deposit_address = UNSET
        else:
            deposit_address = self.deposit_address

        quote: None | str | Unset
        if isinstance(self.quote, Unset):
            quote = UNSET
        else:
            quote = self.quote

        superseded_at: int | None | Unset
        if isinstance(self.superseded_at, Unset):
            superseded_at = UNSET
        else:
            superseded_at = self.superseded_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "chain_id": chain_id,
                "factory": factory,
                "id": id,
                "livemode": livemode,
                "object": object_,
                "salt": salt,
                "treasury": treasury,
            }
        )
        if deposit_address is not UNSET:
            field_dict["deposit_address"] = deposit_address
        if quote is not UNSET:
            field_dict["quote"] = quote
        if superseded_at is not UNSET:
            field_dict["superseded_at"] = superseded_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        chain_id = d.pop("chain_id")

        factory = d.pop("factory")

        id = d.pop("id")

        livemode = d.pop("livemode")

        object_ = d.pop("object")

        salt = d.pop("salt")

        treasury = d.pop("treasury")

        def _parse_deposit_address(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        deposit_address = _parse_deposit_address(d.pop("deposit_address", UNSET))

        def _parse_quote(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        quote = _parse_quote(d.pop("quote", UNSET))

        def _parse_superseded_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        superseded_at = _parse_superseded_at(d.pop("superseded_at", UNSET))

        forwarder = cls(
            address=address,
            chain_id=chain_id,
            factory=factory,
            id=id,
            livemode=livemode,
            object_=object_,
            salt=salt,
            treasury=treasury,
            deposit_address=deposit_address,
            quote=quote,
            superseded_at=superseded_at,
        )

        forwarder.additional_properties = d
        return forwarder

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
