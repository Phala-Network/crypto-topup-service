from typing import Literal

SweepObject = Literal["sweep"]

SWEEP_OBJECT_VALUES: set[SweepObject] = {
    "sweep",
}


def check_sweep_object(value: str) -> SweepObject:
    if value in SWEEP_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {SWEEP_OBJECT_VALUES!r}")
