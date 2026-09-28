from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast


T = TypeVar("T", bound="MarkRefundPaidRequest")


@_attrs_define
class MarkRefundPaidRequest:
    """`POST /v1/refunds/{id}/mark_paid` body: the merchant's refund transaction.

    Example:
        {'receipt_log_index': 0, 'transaction_hash':
            '0x4b6d8f0a2c4e6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b8d0f2a4c6e8b0d2f4a6c'}

    Attributes:
        transaction_hash (str): Hash of the transaction that pays the refund from the treasury of the deposit's address.
        receipt_log_index (int | None | Unset): Position of the `Transfer` log that pays the refund among the logs of
            the transaction's
            receipt (0 for the first), not the block-wide `logIndex`, which changes if the transaction
            is re-included in another block; any matching log when absent.
    """

    transaction_hash: str
    receipt_log_index: int | None | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        transaction_hash = self.transaction_hash

        receipt_log_index: int | None | Unset
        if isinstance(self.receipt_log_index, Unset):
            receipt_log_index = UNSET
        else:
            receipt_log_index = self.receipt_log_index

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "transaction_hash": transaction_hash,
            }
        )
        if receipt_log_index is not UNSET:
            field_dict["receipt_log_index"] = receipt_log_index

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        transaction_hash = d.pop("transaction_hash")

        def _parse_receipt_log_index(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        receipt_log_index = _parse_receipt_log_index(d.pop("receipt_log_index", UNSET))

        mark_refund_paid_request = cls(
            transaction_hash=transaction_hash,
            receipt_log_index=receipt_log_index,
        )

        return mark_refund_paid_request
