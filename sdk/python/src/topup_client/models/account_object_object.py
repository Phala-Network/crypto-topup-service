from typing import Literal

AccountObjectObject = Literal["account"]

ACCOUNT_OBJECT_OBJECT_VALUES: set[AccountObjectObject] = {
    "account",
}


def check_account_object_object(value: str) -> AccountObjectObject:
    if value in ACCOUNT_OBJECT_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {ACCOUNT_OBJECT_OBJECT_VALUES!r}")
