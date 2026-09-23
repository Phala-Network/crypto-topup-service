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
    from ..models.rate_lock_payment import RateLockPayment
    from ..models.rate_lock_salt_inputs import RateLockSaltInputs


T = TypeVar("T", bound="RateLockResponse")


@_attrs_define
class RateLockResponse:
    """Rate-lock response shape owned by C10.

    Attributes:
        address (str): Single-use forwarder address.
        amount_atomic (str): Exact token amount in atomic units.
        credit_minor (str): Destination credit in minor units.
        eip681_uri (str): EIP-681 payment URI.
        expires_at (datetime.datetime): Lock expiry time.
        price_scaled (str): Locked eight-decimal scaled price.
        remaining_seconds (int): Whole seconds remaining in the payment window; zero once `expires_at` has passed.
        salt_inputs (RateLockSaltInputs): Inputs needed to recompute a rate-lock CREATE2 address.
        status (str): Stable lifecycle status: `open`, `consumed`, `expired`, or `cancelled`. A lock stays `open`
            after its window closes until the finalized chain passes `expires_at`, so a payment mined
            inside the window is never reported as expired.
        payment (None | RateLockPayment | Unset):
    """

    address: str
    amount_atomic: str
    credit_minor: str
    eip681_uri: str
    expires_at: datetime.datetime
    price_scaled: str
    remaining_seconds: int
    salt_inputs: RateLockSaltInputs
    status: str
    payment: None | RateLockPayment | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.rate_lock_payment import RateLockPayment  # noqa: PLC0415
        from ..models.rate_lock_salt_inputs import RateLockSaltInputs  # noqa: PLC0415

        address = self.address

        amount_atomic = self.amount_atomic

        credit_minor = self.credit_minor

        eip681_uri = self.eip681_uri

        expires_at = self.expires_at.isoformat()

        price_scaled = self.price_scaled

        remaining_seconds = self.remaining_seconds

        salt_inputs = self.salt_inputs.to_dict()

        status = self.status

        payment: dict[str, Any] | None | Unset
        if isinstance(self.payment, Unset):
            payment = UNSET
        elif isinstance(self.payment, RateLockPayment):
            payment = self.payment.to_dict()
        else:
            payment = self.payment

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "amount_atomic": amount_atomic,
                "credit_minor": credit_minor,
                "eip681_uri": eip681_uri,
                "expires_at": expires_at,
                "price_scaled": price_scaled,
                "remaining_seconds": remaining_seconds,
                "salt_inputs": salt_inputs,
                "status": status,
            }
        )
        if payment is not UNSET:
            field_dict["payment"] = payment

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.rate_lock_payment import RateLockPayment  # noqa: PLC0415
        from ..models.rate_lock_salt_inputs import RateLockSaltInputs  # noqa: PLC0415

        d = dict(src_dict)
        address = d.pop("address")

        amount_atomic = d.pop("amount_atomic")

        credit_minor = d.pop("credit_minor")

        eip681_uri = d.pop("eip681_uri")

        expires_at = datetime.datetime.fromisoformat(d.pop("expires_at"))

        price_scaled = d.pop("price_scaled")

        remaining_seconds = d.pop("remaining_seconds")

        salt_inputs = RateLockSaltInputs.from_dict(d.pop("salt_inputs"))

        status = d.pop("status")

        def _parse_payment(data: object) -> None | RateLockPayment | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                payment_type_1 = RateLockPayment.from_dict(data)

                return payment_type_1
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(None | RateLockPayment | Unset, data)

        payment = _parse_payment(d.pop("payment", UNSET))

        rate_lock_response = cls(
            address=address,
            amount_atomic=amount_atomic,
            credit_minor=credit_minor,
            eip681_uri=eip681_uri,
            expires_at=expires_at,
            price_scaled=price_scaled,
            remaining_seconds=remaining_seconds,
            salt_inputs=salt_inputs,
            status=status,
            payment=payment,
        )

        rate_lock_response.additional_properties = d
        return rate_lock_response

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
