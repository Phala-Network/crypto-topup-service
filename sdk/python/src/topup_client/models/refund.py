from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.deposit import Deposit


T = TypeVar("T", bound="Refund")


@_attrs_define
class Refund:
    """A refund of (part of) a deposit to the customer, executed by finance from the treasury.

    Attributes:
        amount_atomic (str): Token amount in base units, as a decimal string.
        created (int): Request time, Unix seconds.
        deposit (Deposit | str): A deposit id, or the deposit with `expand[]`.
        destination_address (str): Destination address.
        id (str): `re_` id.
        object_ (str): Always `refund`.
        status (str): `pending` (requested, approved, or sent) or `succeeded` (the transfer is final).
        tx_hash (None | str | Unset): Refund transaction hash, once sent.
    """

    amount_atomic: str
    created: int
    deposit: Deposit | str
    destination_address: str
    id: str
    object_: str
    status: str
    tx_hash: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit import Deposit  # noqa: PLC0415

        amount_atomic = self.amount_atomic

        created = self.created

        deposit: dict[str, Any] | str
        if isinstance(self.deposit, Deposit):
            deposit = self.deposit.to_dict()
        else:
            deposit = self.deposit

        destination_address = self.destination_address

        id = self.id

        object_ = self.object_

        status = self.status

        tx_hash: None | str | Unset
        if isinstance(self.tx_hash, Unset):
            tx_hash = UNSET
        else:
            tx_hash = self.tx_hash

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "created": created,
                "deposit": deposit,
                "destination_address": destination_address,
                "id": id,
                "object": object_,
                "status": status,
            }
        )
        if tx_hash is not UNSET:
            field_dict["tx_hash"] = tx_hash

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit import Deposit  # noqa: PLC0415

        d = dict(src_dict)
        amount_atomic = d.pop("amount_atomic")

        created = d.pop("created")

        def _parse_deposit(data: object) -> Deposit | str:
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_expandable_deposit_type_1 = Deposit.from_dict(data)

                return componentsschemas_expandable_deposit_type_1
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(Deposit | str, data)

        deposit = _parse_deposit(d.pop("deposit"))

        destination_address = d.pop("destination_address")

        id = d.pop("id")

        object_ = d.pop("object")

        status = d.pop("status")

        def _parse_tx_hash(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        tx_hash = _parse_tx_hash(d.pop("tx_hash", UNSET))

        refund = cls(
            amount_atomic=amount_atomic,
            created=created,
            deposit=deposit,
            destination_address=destination_address,
            id=id,
            object_=object_,
            status=status,
            tx_hash=tx_hash,
        )

        refund.additional_properties = d
        return refund

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
