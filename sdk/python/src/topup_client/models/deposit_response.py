from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast
from uuid import UUID
import datetime


T = TypeVar("T", bound="DepositResponse")


@_attrs_define
class DepositResponse:
    """Product-visible deposit facts.

    Attributes:
        address (str): Receiving forwarder address.
        amount_atomic (str): Atomic token amount encoded as a decimal string.
        asset_contract (str): Canonical token contract address.
        block_number (int): Finalized block number.
        block_time (datetime.datetime): Finalized block time.
        chain_id (int): EVM chain identifier.
        created_at (datetime.datetime): Row creation time.
        from_address (str): Canonical transfer sender address.
        id (UUID): Deterministic deposit identifier.
        log_index (int): Transfer log index.
        state (str): Current processing state.
        tx_hash (str): Canonical transaction hash.
        updated_at (datetime.datetime): Last processing update time.
        credit_minor (None | str | Unset): Product credit in minor units encoded as a decimal string.
        external_id (str | Unset): Product-owned identifier of the account the receiving address belongs to. This
            service
            always sends it; it is optional in the schema so clients also parse responses from servers
            that predate it.
        lock_ref (None | str | Unset): Rate-lock reference, when applicable.
        price_scaled (None | str | Unset): Eight-decimal scaled price encoded as a decimal string.
        price_source (None | str | Unset): Which price valued the deposit: `lock` (the quoted price) or `spot`.
        route (None | str | Unset): Selected route.
        route_version (int | None | Unset): Selected route version.
        valuation_at (datetime.datetime | None | Unset): Valuation observation time.
    """

    address: str
    amount_atomic: str
    asset_contract: str
    block_number: int
    block_time: datetime.datetime
    chain_id: int
    created_at: datetime.datetime
    from_address: str
    id: UUID
    log_index: int
    state: str
    tx_hash: str
    updated_at: datetime.datetime
    credit_minor: None | str | Unset = UNSET
    external_id: str | Unset = UNSET
    lock_ref: None | str | Unset = UNSET
    price_scaled: None | str | Unset = UNSET
    price_source: None | str | Unset = UNSET
    route: None | str | Unset = UNSET
    route_version: int | None | Unset = UNSET
    valuation_at: datetime.datetime | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        amount_atomic = self.amount_atomic

        asset_contract = self.asset_contract

        block_number = self.block_number

        block_time = self.block_time.isoformat()

        chain_id = self.chain_id

        created_at = self.created_at.isoformat()

        from_address = self.from_address

        id = str(self.id)

        log_index = self.log_index

        state = self.state

        tx_hash = self.tx_hash

        updated_at = self.updated_at.isoformat()

        credit_minor: None | str | Unset
        if isinstance(self.credit_minor, Unset):
            credit_minor = UNSET
        else:
            credit_minor = self.credit_minor

        external_id = self.external_id

        lock_ref: None | str | Unset
        if isinstance(self.lock_ref, Unset):
            lock_ref = UNSET
        else:
            lock_ref = self.lock_ref

        price_scaled: None | str | Unset
        if isinstance(self.price_scaled, Unset):
            price_scaled = UNSET
        else:
            price_scaled = self.price_scaled

        price_source: None | str | Unset
        if isinstance(self.price_source, Unset):
            price_source = UNSET
        else:
            price_source = self.price_source

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

        valuation_at: None | str | Unset
        if isinstance(self.valuation_at, Unset):
            valuation_at = UNSET
        elif isinstance(self.valuation_at, datetime.datetime):
            valuation_at = self.valuation_at.isoformat()
        else:
            valuation_at = self.valuation_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "amount_atomic": amount_atomic,
                "asset_contract": asset_contract,
                "block_number": block_number,
                "block_time": block_time,
                "chain_id": chain_id,
                "created_at": created_at,
                "from_address": from_address,
                "id": id,
                "log_index": log_index,
                "state": state,
                "tx_hash": tx_hash,
                "updated_at": updated_at,
            }
        )
        if credit_minor is not UNSET:
            field_dict["credit_minor"] = credit_minor
        if external_id is not UNSET:
            field_dict["external_id"] = external_id
        if lock_ref is not UNSET:
            field_dict["lock_ref"] = lock_ref
        if price_scaled is not UNSET:
            field_dict["price_scaled"] = price_scaled
        if price_source is not UNSET:
            field_dict["price_source"] = price_source
        if route is not UNSET:
            field_dict["route"] = route
        if route_version is not UNSET:
            field_dict["route_version"] = route_version
        if valuation_at is not UNSET:
            field_dict["valuation_at"] = valuation_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        amount_atomic = d.pop("amount_atomic")

        asset_contract = d.pop("asset_contract")

        block_number = d.pop("block_number")

        block_time = datetime.datetime.fromisoformat(d.pop("block_time"))

        chain_id = d.pop("chain_id")

        created_at = datetime.datetime.fromisoformat(d.pop("created_at"))

        from_address = d.pop("from_address")

        id = UUID(d.pop("id"))

        log_index = d.pop("log_index")

        state = d.pop("state")

        tx_hash = d.pop("tx_hash")

        updated_at = datetime.datetime.fromisoformat(d.pop("updated_at"))

        def _parse_credit_minor(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        credit_minor = _parse_credit_minor(d.pop("credit_minor", UNSET))

        external_id = d.pop("external_id", UNSET)

        def _parse_lock_ref(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        lock_ref = _parse_lock_ref(d.pop("lock_ref", UNSET))

        def _parse_price_scaled(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        price_scaled = _parse_price_scaled(d.pop("price_scaled", UNSET))

        def _parse_price_source(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        price_source = _parse_price_source(d.pop("price_source", UNSET))

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

        def _parse_valuation_at(data: object) -> datetime.datetime | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, str):
                    raise TypeError()
                valuation_at_type_0 = datetime.datetime.fromisoformat(data)

                return valuation_at_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(datetime.datetime | None | Unset, data)

        valuation_at = _parse_valuation_at(d.pop("valuation_at", UNSET))

        deposit_response = cls(
            address=address,
            amount_atomic=amount_atomic,
            asset_contract=asset_contract,
            block_number=block_number,
            block_time=block_time,
            chain_id=chain_id,
            created_at=created_at,
            from_address=from_address,
            id=id,
            log_index=log_index,
            state=state,
            tx_hash=tx_hash,
            updated_at=updated_at,
            credit_minor=credit_minor,
            external_id=external_id,
            lock_ref=lock_ref,
            price_scaled=price_scaled,
            price_source=price_source,
            route=route,
            route_version=route_version,
            valuation_at=valuation_at,
        )

        deposit_response.additional_properties = d
        return deposit_response

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
