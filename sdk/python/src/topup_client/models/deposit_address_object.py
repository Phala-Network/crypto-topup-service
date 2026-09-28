from typing import Literal

DepositAddressObject = Literal["deposit_address"]

DEPOSIT_ADDRESS_OBJECT_VALUES: set[DepositAddressObject] = {
    "deposit_address",
}


def check_deposit_address_object(value: str) -> DepositAddressObject:
    if value in DEPOSIT_ADDRESS_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {DEPOSIT_ADDRESS_OBJECT_VALUES!r}"
    )
