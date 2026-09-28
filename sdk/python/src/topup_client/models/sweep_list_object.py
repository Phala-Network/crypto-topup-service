from typing import Literal

SweepListObject = Literal["list"]

SWEEP_LIST_OBJECT_VALUES: set[SweepListObject] = {
    "list",
}


def check_sweep_list_object(value: str) -> SweepListObject:
    if value in SWEEP_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {SWEEP_LIST_OBJECT_VALUES!r}")
