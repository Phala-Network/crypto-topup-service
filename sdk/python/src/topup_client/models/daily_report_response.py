from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast
import datetime

if TYPE_CHECKING:
    from ..models.route_daily_report import RouteDailyReport


T = TypeVar("T", bound="DailyReportResponse")


@_attrs_define
class DailyReportResponse:
    """Daily finance report produced by C12.

    Attributes:
        generated_at (datetime.datetime): Report snapshot time.
        routes (list[RouteDailyReport]): SQL-computed metrics for each configured route.
        exposure_minor (None | str | Unset): Open rate-lock credit across all products in destination minor units, from
            the global
            exposure counter the global cap is enforced against. This service always sends it; it is
            optional in the schema so clients also parse reports from servers that predate it.
    """

    generated_at: datetime.datetime
    routes: list[RouteDailyReport]
    exposure_minor: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.route_daily_report import RouteDailyReport  # noqa: PLC0415

        generated_at = self.generated_at.isoformat()

        routes = []
        for routes_item_data in self.routes:
            routes_item = routes_item_data.to_dict()
            routes.append(routes_item)

        exposure_minor: None | str | Unset
        if isinstance(self.exposure_minor, Unset):
            exposure_minor = UNSET
        else:
            exposure_minor = self.exposure_minor

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "generated_at": generated_at,
                "routes": routes,
            }
        )
        if exposure_minor is not UNSET:
            field_dict["exposure_minor"] = exposure_minor

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.route_daily_report import RouteDailyReport  # noqa: PLC0415

        d = dict(src_dict)
        generated_at = datetime.datetime.fromisoformat(d.pop("generated_at"))

        routes = []
        _routes = d.pop("routes")
        for routes_item_data in _routes:
            routes_item = RouteDailyReport.from_dict(routes_item_data)

            routes.append(routes_item)

        def _parse_exposure_minor(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        exposure_minor = _parse_exposure_minor(d.pop("exposure_minor", UNSET))

        daily_report_response = cls(
            generated_at=generated_at,
            routes=routes,
            exposure_minor=exposure_minor,
        )

        daily_report_response.additional_properties = d
        return daily_report_response

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
