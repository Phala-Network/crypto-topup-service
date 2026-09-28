from typing import Literal

EventListObject = Literal["list"]

EVENT_LIST_OBJECT_VALUES: set[EventListObject] = {
    "list",
}


def check_event_list_object(value: str) -> EventListObject:
    if value in EVENT_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {EVENT_LIST_OBJECT_VALUES!r}")
