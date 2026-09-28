from typing import Literal

ClientDepositAddressObject = Literal["deposit_address"]

CLIENT_DEPOSIT_ADDRESS_OBJECT_VALUES: set[ClientDepositAddressObject] = {
    "deposit_address",
}


def check_client_deposit_address_object(value: str) -> ClientDepositAddressObject:
    if value in CLIENT_DEPOSIT_ADDRESS_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {CLIENT_DEPOSIT_ADDRESS_OBJECT_VALUES!r}"
    )
