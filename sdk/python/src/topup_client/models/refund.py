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
    from ..models.refund_metadata import RefundMetadata


T = TypeVar("T", bound="Refund")


@_attrs_define
class Refund:
    """A refund of (part of) a deposit to the customer, which the merchant pays from the treasury of
    the deposit's address and attaches with `mark_paid` (design D5).

        Attributes:
            amount_atomic (str): Token amount in base units, as a decimal string.
            created (int): Request time, Unix seconds.
            deposit (Deposit | str): A deposit id, or the deposit with `expand[]`.
            destination_address (str): Destination address.
            id (str): `re_` id.
            livemode (bool): Whether the refund was requested with a live key.
            metadata (RefundMetadata): Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)); `{}` when
                none.
            object_ (str): Always `refund`.
            status (str): `pending` (awaiting payment, or its transaction's finality), `succeeded` (the transfer is
                final), `failed` (the attached transaction does not pay the refund; see
                `failure_reason`), or `canceled`.
            treasury (str): The treasury the refund must be paid from: the one the deposit's address pays, which may
                differ from the account's current treasury.
            failure_reason (None | str | Unset): Why the refund failed: `transaction_failed`, `transfer_not_found`,
                `sender_mismatch`,
                `destination_mismatch`, `amount_mismatch`, or `transfer_already_used`. New values may be
                added.
            log_index (int | None | Unset): Block-wide index of the paying `Transfer` log: as named when marked paid, or
                found at
                verification.
            transaction_hash (None | str | Unset): The attached refund transaction, once marked paid.
    """

    amount_atomic: str
    created: int
    deposit: Deposit | str
    destination_address: str
    id: str
    livemode: bool
    metadata: RefundMetadata
    object_: str
    status: str
    treasury: str
    failure_reason: None | str | Unset = UNSET
    log_index: int | None | Unset = UNSET
    transaction_hash: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit import Deposit  # noqa: PLC0415
        from ..models.refund_metadata import RefundMetadata  # noqa: PLC0415

        amount_atomic = self.amount_atomic

        created = self.created

        deposit: dict[str, Any] | str
        if isinstance(self.deposit, Deposit):
            deposit = self.deposit.to_dict()
        else:
            deposit = self.deposit

        destination_address = self.destination_address

        id = self.id

        livemode = self.livemode

        metadata = self.metadata.to_dict()

        object_ = self.object_

        status = self.status

        treasury = self.treasury

        failure_reason: None | str | Unset
        if isinstance(self.failure_reason, Unset):
            failure_reason = UNSET
        else:
            failure_reason = self.failure_reason

        log_index: int | None | Unset
        if isinstance(self.log_index, Unset):
            log_index = UNSET
        else:
            log_index = self.log_index

        transaction_hash: None | str | Unset
        if isinstance(self.transaction_hash, Unset):
            transaction_hash = UNSET
        else:
            transaction_hash = self.transaction_hash

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "amount_atomic": amount_atomic,
                "created": created,
                "deposit": deposit,
                "destination_address": destination_address,
                "id": id,
                "livemode": livemode,
                "metadata": metadata,
                "object": object_,
                "status": status,
                "treasury": treasury,
            }
        )
        if failure_reason is not UNSET:
            field_dict["failure_reason"] = failure_reason
        if log_index is not UNSET:
            field_dict["log_index"] = log_index
        if transaction_hash is not UNSET:
            field_dict["transaction_hash"] = transaction_hash

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit import Deposit  # noqa: PLC0415
        from ..models.refund_metadata import RefundMetadata  # noqa: PLC0415

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

        livemode = d.pop("livemode")

        metadata = RefundMetadata.from_dict(d.pop("metadata"))

        object_ = d.pop("object")

        status = d.pop("status")

        treasury = d.pop("treasury")

        def _parse_failure_reason(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        failure_reason = _parse_failure_reason(d.pop("failure_reason", UNSET))

        def _parse_log_index(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        log_index = _parse_log_index(d.pop("log_index", UNSET))

        def _parse_transaction_hash(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        transaction_hash = _parse_transaction_hash(d.pop("transaction_hash", UNSET))

        refund = cls(
            amount_atomic=amount_atomic,
            created=created,
            deposit=deposit,
            destination_address=destination_address,
            id=id,
            livemode=livemode,
            metadata=metadata,
            object_=object_,
            status=status,
            treasury=treasury,
            failure_reason=failure_reason,
            log_index=log_index,
            transaction_hash=transaction_hash,
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
