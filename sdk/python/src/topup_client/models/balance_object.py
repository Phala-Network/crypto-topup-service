from typing import Literal

BalanceObject = Literal["balance"]

BALANCE_OBJECT_VALUES: set[BalanceObject] = {
    "balance",
}


def check_balance_object(value: str) -> BalanceObject:
    if value in BALANCE_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {BALANCE_OBJECT_VALUES!r}")
