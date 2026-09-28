from typing import Literal

EventObjectResponseObject = Literal["event"]

EVENT_OBJECT_RESPONSE_OBJECT_VALUES: set[EventObjectResponseObject] = {
    "event",
}


def check_event_object_response_object(value: str) -> EventObjectResponseObject:
    if value in EVENT_OBJECT_RESPONSE_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {EVENT_OBJECT_RESPONSE_OBJECT_VALUES!r}"
    )
