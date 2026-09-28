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
        {'log_index': 41, 'transaction_hash': '0x4b6d8f0a2c4e6a8c0e2b4d6f8a0c2e4b6d8f0a2c4e6b8d0f2a4c6e8b0d2f4a6c'}

    Attributes:
        transaction_hash (str): Hash of the transaction that pays the refund from the treasury of the deposit's address.
        log_index (int | None | Unset): Block-wide index of the `Transfer` log that pays the refund; any matching log
            when absent.
    """

    transaction_hash: str
    log_index: int | None | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        transaction_hash = self.transaction_hash

        log_index: int | None | Unset
        if isinstance(self.log_index, Unset):
            log_index = UNSET
        else:
            log_index = self.log_index

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "transaction_hash": transaction_hash,
            }
        )
        if log_index is not UNSET:
            field_dict["log_index"] = log_index

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        transaction_hash = d.pop("transaction_hash")

        def _parse_log_index(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        log_index = _parse_log_index(d.pop("log_index", UNSET))

        mark_refund_paid_request = cls(
            transaction_hash=transaction_hash,
            log_index=log_index,
        )

        return mark_refund_paid_request
