from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="ConfirmationPolicy")


@_attrs_define
class ConfirmationPolicy:
    """One chain's confirmation the account requires (design D1).

    Attributes:
        chain_id (int): A chain of the key's mode (`GET /v1/config`).
        confirmations (None | str | Unset): A depth (`"12"`: the block and eleven more), `"safe"`, or `"finalized"`:
            never weaker than
            the route's `confirmations` (any depth < `safe` < `finalized`), and of the chain's kind (a
            depth or `finalized` on Ethereum; a depth, `safe`, or `finalized` on an OP-stack chain). In
            a request, `null` removes the chain's policy, so its route's applies.
    """

    chain_id: int
    confirmations: None | str | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        chain_id = self.chain_id

        confirmations: None | str | Unset
        if isinstance(self.confirmations, Unset):
            confirmations = UNSET
        else:
            confirmations = self.confirmations

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "chain_id": chain_id,
            }
        )
        if confirmations is not UNSET:
            field_dict["confirmations"] = confirmations

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        chain_id = d.pop("chain_id")

        def _parse_confirmations(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        confirmations = _parse_confirmations(d.pop("confirmations", UNSET))

        confirmation_policy = cls(
            chain_id=chain_id,
            confirmations=confirmations,
        )

        return confirmation_policy
