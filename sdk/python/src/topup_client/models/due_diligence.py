from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from typing import cast
import datetime


T = TypeVar("T", bound="DueDiligence")


@_attrs_define
class DueDiligence:
    """The record of the operator's offline due diligence (design D8): a reference to it, when, and
    by whom.

        Attributes:
            reference (str): Reference to the review in Phala's records, 1 to 200 characters.
            reviewed_at (datetime.date): Date of the review, `YYYY-MM-DD`.
            reviewed_by (str): Who reviewed, 1 to 200 characters.
    """

    reference: str
    reviewed_at: datetime.date
    reviewed_by: str

    def to_dict(self) -> dict[str, Any]:
        reference = self.reference

        reviewed_at = self.reviewed_at.isoformat()

        reviewed_by = self.reviewed_by

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "reference": reference,
                "reviewed_at": reviewed_at,
                "reviewed_by": reviewed_by,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        reference = d.pop("reference")

        reviewed_at = datetime.date.fromisoformat(d.pop("reviewed_at"))

        reviewed_by = d.pop("reviewed_by")

        due_diligence = cls(
            reference=reference,
            reviewed_at=reviewed_at,
            reviewed_by=reviewed_by,
        )

        return due_diligence
