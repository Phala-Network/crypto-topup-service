from typing import Literal

DepositAddressListObject = Literal["list"]

DEPOSIT_ADDRESS_LIST_OBJECT_VALUES: set[DepositAddressListObject] = {
    "list",
}


def check_deposit_address_list_object(value: str) -> DepositAddressListObject:
    if value in DEPOSIT_ADDRESS_LIST_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {DEPOSIT_ADDRESS_LIST_OBJECT_VALUES!r}"
    )
