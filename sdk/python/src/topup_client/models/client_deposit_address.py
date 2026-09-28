from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.client_deposit_address_object import check_client_deposit_address_object
from ..models.client_deposit_address_object import ClientDepositAddressObject
from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.client_deposit_address_network import ClientDepositAddressNetwork
    from ..models.client_deposit_address_payment import ClientDepositAddressPayment


T = TypeVar("T", bound="ClientDepositAddress")


@_attrs_define
class ClientDepositAddress:
    """The public view of a deposit address, read with its `client_secret` and without an API key,
    for the customer's page. It has no account, customer, treasury, or metadata fields.

        Attributes:
            id (str): `da_` id.
            livemode (bool): Whether the address is in live mode; a test-mode page should say so.
            networks (list[ClientDepositAddressNetwork]): The address on each supported network, with the tokens it takes
                there.
            object_ (ClientDepositAddressObject): Always `deposit_address`.
            payments (list[ClientDepositAddressPayment]): Payments to the address in the last 24 hours, newest first, at
                most 10: display only,
                never a reason to deliver anything.
            status (str): `active`, or `retired` by a rotation (still credited; show the new address instead).
            address (None | str | Unset): The address shared by every network, or `null` when a network's differs.
    """

    id: str
    livemode: bool
    networks: list[ClientDepositAddressNetwork]
    object_: ClientDepositAddressObject
    payments: list[ClientDepositAddressPayment]
    status: str
    address: None | str | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.client_deposit_address_network import ClientDepositAddressNetwork  # noqa: PLC0415
        from ..models.client_deposit_address_payment import ClientDepositAddressPayment  # noqa: PLC0415

        id = self.id

        livemode = self.livemode

        networks = []
        for networks_item_data in self.networks:
            networks_item = networks_item_data.to_dict()
            networks.append(networks_item)

        object_: str = self.object_

        payments = []
        for payments_item_data in self.payments:
            payments_item = payments_item_data.to_dict()
            payments.append(payments_item)

        status = self.status

        address: None | str | Unset
        if isinstance(self.address, Unset):
            address = UNSET
        else:
            address = self.address

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "id": id,
                "livemode": livemode,
                "networks": networks,
                "object": object_,
                "payments": payments,
                "status": status,
            }
        )
        if address is not UNSET:
            field_dict["address"] = address

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.client_deposit_address_network import ClientDepositAddressNetwork  # noqa: PLC0415
        from ..models.client_deposit_address_payment import ClientDepositAddressPayment  # noqa: PLC0415

        d = dict(src_dict)
        id = d.pop("id")

        livemode = d.pop("livemode")

        networks = []
        _networks = d.pop("networks")
        for networks_item_data in _networks:
            networks_item = ClientDepositAddressNetwork.from_dict(networks_item_data)

            networks.append(networks_item)

        object_ = check_client_deposit_address_object(d.pop("object"))

        payments = []
        _payments = d.pop("payments")
        for payments_item_data in _payments:
            payments_item = ClientDepositAddressPayment.from_dict(payments_item_data)

            payments.append(payments_item)

        status = d.pop("status")

        def _parse_address(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        address = _parse_address(d.pop("address", UNSET))

        client_deposit_address = cls(
            id=id,
            livemode=livemode,
            networks=networks,
            object_=object_,
            payments=payments,
            status=status,
            address=address,
        )

        client_deposit_address.additional_properties = d
        return client_deposit_address

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
