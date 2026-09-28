from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="CreateTreasuryChallengeRequest")


@_attrs_define
class CreateTreasuryChallengeRequest:
    """`POST /v1/treasuries/challenge` body.

    Example:
        {'address': '0x936c1991f8da9a919fa11b557a3514719f5a4504', 'chain_id': 1}

    Attributes:
        address (str): The treasury address to prove: an EOA, or a contract deployed on the chain such as a Safe.
        chain_id (int): The chain of the treasury: a chain of the key's mode (`GET /v1/config`).
    """

    address: str
    chain_id: int

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        chain_id = self.chain_id

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "address": address,
                "chain_id": chain_id,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        chain_id = d.pop("chain_id")

        create_treasury_challenge_request = cls(
            address=address,
            chain_id=chain_id,
        )

        return create_treasury_challenge_request
