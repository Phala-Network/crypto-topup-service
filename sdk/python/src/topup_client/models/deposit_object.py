from typing import Literal

DepositObject = Literal["deposit"]

DEPOSIT_OBJECT_VALUES: set[DepositObject] = {
    "deposit",
}


def check_deposit_object(value: str) -> DepositObject:
    if value in DEPOSIT_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {DEPOSIT_OBJECT_VALUES!r}")
