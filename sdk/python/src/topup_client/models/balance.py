from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast

if TYPE_CHECKING:
    from ..models.balance_amount import BalanceAmount


T = TypeVar("T", bound="Balance")


@_attrs_define
class Balance:
    """The account's balance held in its forwarders, in the key's mode (Stripe's Balance): what
    payments put there and no finalized `Flushed` event has moved to a treasury yet.

        Attributes:
            livemode (bool): The mode.
            object_ (str): Always `balance`.
            unswept (list[BalanceAmount]): One entry per chain and token held.
    """

    livemode: bool
    object_: str
    unswept: list[BalanceAmount]
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.balance_amount import BalanceAmount  # noqa: PLC0415

        livemode = self.livemode

        object_ = self.object_

        unswept = []
        for unswept_item_data in self.unswept:
            unswept_item = unswept_item_data.to_dict()
            unswept.append(unswept_item)

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "livemode": livemode,
                "object": object_,
                "unswept": unswept,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.balance_amount import BalanceAmount  # noqa: PLC0415

        d = dict(src_dict)
        livemode = d.pop("livemode")

        object_ = d.pop("object")

        unswept = []
        _unswept = d.pop("unswept")
        for unswept_item_data in _unswept:
            unswept_item = BalanceAmount.from_dict(unswept_item_data)

            unswept.append(unswept_item)

        balance = cls(
            livemode=livemode,
            object_=object_,
            unswept=unswept,
        )

        balance.additional_properties = d
        return balance

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
