from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast
import datetime


T = TypeVar("T", bound="LimitsResponse")


@_attrs_define
class LimitsResponse:
    """Configured route limits and currently available exposure.

    Attributes:
        account_open_minor (int): Per-account open rate-lock cap in minor units.
        global_open_minor (int): Global open rate-lock cap in minor units.
        max_deposit_atomic (str): Maximum atomic deposit amount.
        min_credit_minor (int): Minimum destination credit in minor units.
        min_deposit_atomic (str): Minimum atomic deposit amount.
        product_open_minor (int): Per-product open rate-lock cap in minor units.
        route (str): Route name.
        remaining_account_minor (int | None | Unset): Remaining account exposure.
        reset_at (datetime.datetime | None | Unset): Earliest open-lock expiry, when any exposure is reserved.
    """

    account_open_minor: int
    global_open_minor: int
    max_deposit_atomic: str
    min_credit_minor: int
    min_deposit_atomic: str
    product_open_minor: int
    route: str
    remaining_account_minor: int | None | Unset = UNSET
    reset_at: datetime.datetime | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        account_open_minor = self.account_open_minor

        global_open_minor = self.global_open_minor

        max_deposit_atomic = self.max_deposit_atomic

        min_credit_minor = self.min_credit_minor

        min_deposit_atomic = self.min_deposit_atomic

        product_open_minor = self.product_open_minor

        route = self.route

        remaining_account_minor: int | None | Unset
        if isinstance(self.remaining_account_minor, Unset):
            remaining_account_minor = UNSET
        else:
            remaining_account_minor = self.remaining_account_minor

        reset_at: None | str | Unset
        if isinstance(self.reset_at, Unset):
            reset_at = UNSET
        elif isinstance(self.reset_at, datetime.datetime):
            reset_at = self.reset_at.isoformat()
        else:
            reset_at = self.reset_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "account_open_minor": account_open_minor,
                "global_open_minor": global_open_minor,
                "max_deposit_atomic": max_deposit_atomic,
                "min_credit_minor": min_credit_minor,
                "min_deposit_atomic": min_deposit_atomic,
                "product_open_minor": product_open_minor,
                "route": route,
            }
        )
        if remaining_account_minor is not UNSET:
            field_dict["remaining_account_minor"] = remaining_account_minor
        if reset_at is not UNSET:
            field_dict["reset_at"] = reset_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        account_open_minor = d.pop("account_open_minor")

        global_open_minor = d.pop("global_open_minor")

        max_deposit_atomic = d.pop("max_deposit_atomic")

        min_credit_minor = d.pop("min_credit_minor")

        min_deposit_atomic = d.pop("min_deposit_atomic")

        product_open_minor = d.pop("product_open_minor")

        route = d.pop("route")

        def _parse_remaining_account_minor(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        remaining_account_minor = _parse_remaining_account_minor(
            d.pop("remaining_account_minor", UNSET)
        )

        def _parse_reset_at(data: object) -> datetime.datetime | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, str):
                    raise TypeError()
                reset_at_type_0 = datetime.datetime.fromisoformat(data)

                return reset_at_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(datetime.datetime | None | Unset, data)

        reset_at = _parse_reset_at(d.pop("reset_at", UNSET))

        limits_response = cls(
            account_open_minor=account_open_minor,
            global_open_minor=global_open_minor,
            max_deposit_atomic=max_deposit_atomic,
            min_credit_minor=min_credit_minor,
            min_deposit_atomic=min_deposit_atomic,
            product_open_minor=product_open_minor,
            route=route,
            remaining_account_minor=remaining_account_minor,
            reset_at=reset_at,
        )

        limits_response.additional_properties = d
        return limits_response

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
