from typing import Literal

TreasuryObject = Literal["treasury"]

TREASURY_OBJECT_VALUES: set[TreasuryObject] = {
    "treasury",
}


def check_treasury_object(value: str) -> TreasuryObject:
    if value in TREASURY_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {TREASURY_OBJECT_VALUES!r}")
