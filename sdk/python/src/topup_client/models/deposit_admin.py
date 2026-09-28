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
    from ..models.deposit_event_delivery import DepositEventDelivery
    from ..models.deposit_transition import DepositTransition


T = TypeVar("T", bound="DepositAdmin")


@_attrs_define
class DepositAdmin:
    """The operator's view of a deposit's internals (`GET /v1/admin/deposits/{id}`), returned as the
    `admin` field of the deposit; never present in a merchant response or an event.

        Attributes:
            account (str): The merchant account the deposit belongs to, `acct_…`.
            block_time (datetime.datetime): Including block time.
            events (list[DepositEventDelivery]): Events about the deposit in ascending creation order, with their delivery.
            receipt_log_index (int): Position of the transfer log in its transaction's receipt; with the chain and
                transaction,
                the deposit's identity.
            state (str): The processing state: `detected`, `confirmed`, `credited`, `swept`, `rejected`, or
                `reversed` (`status` is its merchant view).
            transitions (list[DepositTransition]): Transitions in ascending creation order.
            updated_at (datetime.datetime): Last processing update time.
            final_at (datetime.datetime | None | Unset): When both providers showed the transfer at or below `finalized`.
            price_scaled (None | str | Unset): Eight-decimal scaled price as a decimal string.
            route (None | str | Unset): Selected route.
            route_version (int | None | Unset): Selected route version.
    """

    account: str
    block_time: datetime.datetime
    events: list[DepositEventDelivery]
    receipt_log_index: int
    state: str
    transitions: list[DepositTransition]
    updated_at: datetime.datetime
    final_at: datetime.datetime | None | Unset = UNSET
    price_scaled: None | str | Unset = UNSET
    route: None | str | Unset = UNSET
    route_version: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit_event_delivery import DepositEventDelivery  # noqa: PLC0415
        from ..models.deposit_transition import DepositTransition  # noqa: PLC0415

        account = self.account

        block_time = self.block_time.isoformat()

        events = []
        for events_item_data in self.events:
            events_item = events_item_data.to_dict()
            events.append(events_item)

        receipt_log_index = self.receipt_log_index

        state = self.state

        transitions = []
        for transitions_item_data in self.transitions:
            transitions_item = transitions_item_data.to_dict()
            transitions.append(transitions_item)

        updated_at = self.updated_at.isoformat()

        final_at: None | str | Unset
        if isinstance(self.final_at, Unset):
            final_at = UNSET
        elif isinstance(self.final_at, datetime.datetime):
            final_at = self.final_at.isoformat()
        else:
            final_at = self.final_at

        price_scaled: None | str | Unset
        if isinstance(self.price_scaled, Unset):
            price_scaled = UNSET
        else:
            price_scaled = self.price_scaled

        route: None | str | Unset
        if isinstance(self.route, Unset):
            route = UNSET
        else:
            route = self.route

        route_version: int | None | Unset
        if isinstance(self.route_version, Unset):
            route_version = UNSET
        else:
            route_version = self.route_version

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "account": account,
                "block_time": block_time,
                "events": events,
                "receipt_log_index": receipt_log_index,
                "state": state,
                "transitions": transitions,
                "updated_at": updated_at,
            }
        )
        if final_at is not UNSET:
            field_dict["final_at"] = final_at
        if price_scaled is not UNSET:
            field_dict["price_scaled"] = price_scaled
        if route is not UNSET:
            field_dict["route"] = route
        if route_version is not UNSET:
            field_dict["route_version"] = route_version

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit_event_delivery import DepositEventDelivery  # noqa: PLC0415
        from ..models.deposit_transition import DepositTransition  # noqa: PLC0415

        d = dict(src_dict)
        account = d.pop("account")

        block_time = datetime.datetime.fromisoformat(d.pop("block_time"))

        events = []
        _events = d.pop("events")
        for events_item_data in _events:
            events_item = DepositEventDelivery.from_dict(events_item_data)

            events.append(events_item)

        receipt_log_index = d.pop("receipt_log_index")

        state = d.pop("state")

        transitions = []
        _transitions = d.pop("transitions")
        for transitions_item_data in _transitions:
            transitions_item = DepositTransition.from_dict(transitions_item_data)

            transitions.append(transitions_item)

        updated_at = datetime.datetime.fromisoformat(d.pop("updated_at"))

        def _parse_final_at(data: object) -> datetime.datetime | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, str):
                    raise TypeError()
                final_at_type_0 = datetime.datetime.fromisoformat(data)

                return final_at_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(datetime.datetime | None | Unset, data)

        final_at = _parse_final_at(d.pop("final_at", UNSET))

        def _parse_price_scaled(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        price_scaled = _parse_price_scaled(d.pop("price_scaled", UNSET))

        def _parse_route(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        route = _parse_route(d.pop("route", UNSET))

        def _parse_route_version(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        route_version = _parse_route_version(d.pop("route_version", UNSET))

        deposit_admin = cls(
            account=account,
            block_time=block_time,
            events=events,
            receipt_log_index=receipt_log_index,
            state=state,
            transitions=transitions,
            updated_at=updated_at,
            final_at=final_at,
            price_scaled=price_scaled,
            route=route,
            route_version=route_version,
        )

        deposit_admin.additional_properties = d
        return deposit_admin

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
