from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast
import datetime

if TYPE_CHECKING:
    from ..models.failed_check_report import FailedCheckReport


T = TypeVar("T", bound="ReconciliationRoundReport")


@_attrs_define
class ReconciliationRoundReport:
    """Latest reconciliation round of the serving process.

    Attributes:
        at (datetime.datetime): When the round finished.
        failed_checks (list[FailedCheckReport]): Checks that could not complete; empty after a complete round.
    """

    at: datetime.datetime
    failed_checks: list[FailedCheckReport]
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.failed_check_report import FailedCheckReport  # noqa: PLC0415

        at = self.at.isoformat()

        failed_checks = []
        for failed_checks_item_data in self.failed_checks:
            failed_checks_item = failed_checks_item_data.to_dict()
            failed_checks.append(failed_checks_item)

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "at": at,
                "failed_checks": failed_checks,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.failed_check_report import FailedCheckReport  # noqa: PLC0415

        d = dict(src_dict)
        at = datetime.datetime.fromisoformat(d.pop("at"))

        failed_checks = []
        _failed_checks = d.pop("failed_checks")
        for failed_checks_item_data in _failed_checks:
            failed_checks_item = FailedCheckReport.from_dict(failed_checks_item_data)

            failed_checks.append(failed_checks_item)

        reconciliation_round_report = cls(
            at=at,
            failed_checks=failed_checks,
        )

        reconciliation_round_report.additional_properties = d
        return reconciliation_round_report

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
