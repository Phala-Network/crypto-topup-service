from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="CreateRateLockRequest")


@_attrs_define
class CreateRateLockRequest:
    """Rate-lock creation body owned by C10.

    Attributes:
        product_lock_ref (str): Product checkout reference.
        amount_atomic (None | str | Unset): Desired token amount in atomic units.
        amount_minor (None | str | Unset): Desired destination amount in minor units.
    """

    product_lock_ref: str
    amount_atomic: None | str | Unset = UNSET
    amount_minor: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        product_lock_ref = self.product_lock_ref

        amount_atomic: None | str | Unset
        if isinstance(self.amount_atomic, Unset):
            amount_atomic = UNSET
        else:
            amount_atomic = self.amount_atomic

        amount_minor: None | str | Unset
        if isinstance(self.amount_minor, Unset):
            amount_minor = UNSET
        else:
            amount_minor = self.amount_minor

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "product_lock_ref": product_lock_ref,
            }
        )
        if amount_atomic is not UNSET:
            field_dict["amount_atomic"] = amount_atomic
        if amount_minor is not UNSET:
            field_dict["amount_minor"] = amount_minor

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        product_lock_ref = d.pop("product_lock_ref")

        def _parse_amount_atomic(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        amount_atomic = _parse_amount_atomic(d.pop("amount_atomic", UNSET))

        def _parse_amount_minor(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        amount_minor = _parse_amount_minor(d.pop("amount_minor", UNSET))

        create_rate_lock_request = cls(
            product_lock_ref=product_lock_ref,
            amount_atomic=amount_atomic,
            amount_minor=amount_minor,
        )

        create_rate_lock_request.additional_properties = d
        return create_rate_lock_request

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
