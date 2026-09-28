from typing import Literal

ForwarderListObject = Literal["list"]

FORWARDER_LIST_OBJECT_VALUES: set[ForwarderListObject] = {
    "list",
}


def check_forwarder_list_object(value: str) -> ForwarderListObject:
    if value in FORWARDER_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {FORWARDER_LIST_OBJECT_VALUES!r}")
