from typing import Literal

TreasuryListObject = Literal["list"]

TREASURY_LIST_OBJECT_VALUES: set[TreasuryListObject] = {
    "list",
}


def check_treasury_list_object(value: str) -> TreasuryListObject:
    if value in TREASURY_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {TREASURY_LIST_OBJECT_VALUES!r}")
