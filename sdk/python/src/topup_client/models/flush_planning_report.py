from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast
import datetime


T = TypeVar("T", bound="FlushPlanningReport")


@_attrs_define
class FlushPlanningReport:
    """Outcome of one scheduled flush planning run.

    Attributes:
        at (datetime.datetime): When the run finished.
        outcome (str): `planned`; `idle` when no address met the flush policy, a flush was already in flight, or
            reconciliation froze the chain; `operator_not_authorized` when the operator was not known
            to hold `OPERATOR_ROLE`; `failed`; or `send_failed` when planning succeeded but sending or
            maintaining a flush afterwards failed.
        error (None | str | Unset): Error of a failed run, without provider URLs.
    """

    at: datetime.datetime
    outcome: str
    error: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        at = self.at.isoformat()

        outcome = self.outcome

        error: None | str | Unset
        if isinstance(self.error, Unset):
            error = UNSET
        else:
            error = self.error

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "at": at,
                "outcome": outcome,
            }
        )
        if error is not UNSET:
            field_dict["error"] = error

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        at = datetime.datetime.fromisoformat(d.pop("at"))

        outcome = d.pop("outcome")

        def _parse_error(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        error = _parse_error(d.pop("error", UNSET))

        flush_planning_report = cls(
            at=at,
            outcome=outcome,
            error=error,
        )

        flush_planning_report.additional_properties = d
        return flush_planning_report

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
