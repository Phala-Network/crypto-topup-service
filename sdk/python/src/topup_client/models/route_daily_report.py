from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.flush_planning_report import FlushPlanningReport
    from ..models.route_daily_report_age_in_state_max_seconds import (
        RouteDailyReportAgeInStateMaxSeconds,
    )
    from ..models.route_daily_report_deposits_by_state import RouteDailyReportDepositsByState
    from ..models.route_daily_report_refunds_by_status import RouteDailyReportRefundsByStatus


T = TypeVar("T", bound="RouteDailyReport")


@_attrs_define
class RouteDailyReport:
    """Per-route daily finance report produced by C12.

    Attributes:
        age_in_state_max_seconds (RouteDailyReportAgeInStateMaxSeconds): Maximum age in seconds keyed by current deposit
            state.
        asset_contract (str): Route asset contract.
        chain_id (int): EVM chain identifier.
        credited_undelivered (int): Credited deposits whose `deposit.credited` webhook the product has not acknowledged
            yet.
        credited_undelivered_max_age_seconds (int): Age in seconds of the oldest of those events; zero when every one
            was delivered.
        deposits_by_state (RouteDailyReportDepositsByState): Deposit counts keyed by state.
        open_rate_lock_exposure_atomic (str): Sum of unconsumed rate-lock token amounts.
        refunds_by_status (RouteDailyReportRefundsByStatus): Refund counts keyed by status.
        rejected_holds_atomic (str): Rejected token amount still held after confirmed refunds.
        route (str): Stable route name.
        treasury_balance_note (str): Balance source or explicit reason the treasury balance is unavailable.
        unflushed_balance_atomic (str): Sum of deposits not linked to a confirmed flush.
        flush_planning (FlushPlanningReport | None | Unset):
        treasury_balance_atomic (None | str | Unset): Latest treasury token balance in atomic units, when the chain read
            succeeds.
    """

    age_in_state_max_seconds: RouteDailyReportAgeInStateMaxSeconds
    asset_contract: str
    chain_id: int
    credited_undelivered: int
    credited_undelivered_max_age_seconds: int
    deposits_by_state: RouteDailyReportDepositsByState
    open_rate_lock_exposure_atomic: str
    refunds_by_status: RouteDailyReportRefundsByStatus
    rejected_holds_atomic: str
    route: str
    treasury_balance_note: str
    unflushed_balance_atomic: str
    flush_planning: FlushPlanningReport | None | Unset = UNSET
    treasury_balance_atomic: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.flush_planning_report import FlushPlanningReport  # noqa: PLC0415
        from ..models.route_daily_report_age_in_state_max_seconds import (
            RouteDailyReportAgeInStateMaxSeconds,
        )  # noqa: PLC0415
        from ..models.route_daily_report_deposits_by_state import RouteDailyReportDepositsByState  # noqa: PLC0415
        from ..models.route_daily_report_refunds_by_status import RouteDailyReportRefundsByStatus  # noqa: PLC0415

        age_in_state_max_seconds = self.age_in_state_max_seconds.to_dict()

        asset_contract = self.asset_contract

        chain_id = self.chain_id

        credited_undelivered = self.credited_undelivered

        credited_undelivered_max_age_seconds = self.credited_undelivered_max_age_seconds

        deposits_by_state = self.deposits_by_state.to_dict()

        open_rate_lock_exposure_atomic = self.open_rate_lock_exposure_atomic

        refunds_by_status = self.refunds_by_status.to_dict()

        rejected_holds_atomic = self.rejected_holds_atomic

        route = self.route

        treasury_balance_note = self.treasury_balance_note

        unflushed_balance_atomic = self.unflushed_balance_atomic

        flush_planning: dict[str, Any] | None | Unset
        if isinstance(self.flush_planning, Unset):
            flush_planning = UNSET
        elif isinstance(self.flush_planning, FlushPlanningReport):
            flush_planning = self.flush_planning.to_dict()
        else:
            flush_planning = self.flush_planning

        treasury_balance_atomic: None | str | Unset
        if isinstance(self.treasury_balance_atomic, Unset):
            treasury_balance_atomic = UNSET
        else:
            treasury_balance_atomic = self.treasury_balance_atomic

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "age_in_state_max_seconds": age_in_state_max_seconds,
                "asset_contract": asset_contract,
                "chain_id": chain_id,
                "credited_undelivered": credited_undelivered,
                "credited_undelivered_max_age_seconds": credited_undelivered_max_age_seconds,
                "deposits_by_state": deposits_by_state,
                "open_rate_lock_exposure_atomic": open_rate_lock_exposure_atomic,
                "refunds_by_status": refunds_by_status,
                "rejected_holds_atomic": rejected_holds_atomic,
                "route": route,
                "treasury_balance_note": treasury_balance_note,
                "unflushed_balance_atomic": unflushed_balance_atomic,
            }
        )
        if flush_planning is not UNSET:
            field_dict["flush_planning"] = flush_planning
        if treasury_balance_atomic is not UNSET:
            field_dict["treasury_balance_atomic"] = treasury_balance_atomic

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.flush_planning_report import FlushPlanningReport  # noqa: PLC0415
        from ..models.route_daily_report_age_in_state_max_seconds import (
            RouteDailyReportAgeInStateMaxSeconds,
        )  # noqa: PLC0415
        from ..models.route_daily_report_deposits_by_state import RouteDailyReportDepositsByState  # noqa: PLC0415
        from ..models.route_daily_report_refunds_by_status import RouteDailyReportRefundsByStatus  # noqa: PLC0415

        d = dict(src_dict)
        age_in_state_max_seconds = RouteDailyReportAgeInStateMaxSeconds.from_dict(
            d.pop("age_in_state_max_seconds")
        )

        asset_contract = d.pop("asset_contract")

        chain_id = d.pop("chain_id")

        credited_undelivered = d.pop("credited_undelivered")

        credited_undelivered_max_age_seconds = d.pop("credited_undelivered_max_age_seconds")

        deposits_by_state = RouteDailyReportDepositsByState.from_dict(d.pop("deposits_by_state"))

        open_rate_lock_exposure_atomic = d.pop("open_rate_lock_exposure_atomic")

        refunds_by_status = RouteDailyReportRefundsByStatus.from_dict(d.pop("refunds_by_status"))

        rejected_holds_atomic = d.pop("rejected_holds_atomic")

        route = d.pop("route")

        treasury_balance_note = d.pop("treasury_balance_note")

        unflushed_balance_atomic = d.pop("unflushed_balance_atomic")

        def _parse_flush_planning(data: object) -> FlushPlanningReport | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                flush_planning_type_0 = FlushPlanningReport.from_dict(data)

                return flush_planning_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(FlushPlanningReport | None | Unset, data)

        flush_planning = _parse_flush_planning(d.pop("flush_planning", UNSET))

        def _parse_treasury_balance_atomic(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        treasury_balance_atomic = _parse_treasury_balance_atomic(
            d.pop("treasury_balance_atomic", UNSET)
        )

        route_daily_report = cls(
            age_in_state_max_seconds=age_in_state_max_seconds,
            asset_contract=asset_contract,
            chain_id=chain_id,
            credited_undelivered=credited_undelivered,
            credited_undelivered_max_age_seconds=credited_undelivered_max_age_seconds,
            deposits_by_state=deposits_by_state,
            open_rate_lock_exposure_atomic=open_rate_lock_exposure_atomic,
            refunds_by_status=refunds_by_status,
            rejected_holds_atomic=rejected_holds_atomic,
            route=route,
            treasury_balance_note=treasury_balance_note,
            unflushed_balance_atomic=unflushed_balance_atomic,
            flush_planning=flush_planning,
            treasury_balance_atomic=treasury_balance_atomic,
        )

        route_daily_report.additional_properties = d
        return route_daily_report

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
