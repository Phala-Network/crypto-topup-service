from typing import Literal

ForwarderObject = Literal["forwarder"]

FORWARDER_OBJECT_VALUES: set[ForwarderObject] = {
    "forwarder",
}


def check_forwarder_object(value: str) -> ForwarderObject:
    if value in FORWARDER_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {FORWARDER_OBJECT_VALUES!r}")
