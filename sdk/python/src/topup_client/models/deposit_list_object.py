from typing import Literal

DepositListObject = Literal["list"]

DEPOSIT_LIST_OBJECT_VALUES: set[DepositListObject] = {
    "list",
}


def check_deposit_list_object(value: str) -> DepositListObject:
    if value in DEPOSIT_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {DEPOSIT_LIST_OBJECT_VALUES!r}")
