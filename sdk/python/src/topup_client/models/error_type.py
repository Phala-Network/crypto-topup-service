from typing import Literal

ErrorType = Literal["api_error", "idempotency_error", "invalid_request_error"]

ERROR_TYPE_VALUES: set[ErrorType] = {
    "api_error",
    "idempotency_error",
    "invalid_request_error",
}


def check_error_type(value: str) -> ErrorType:
    if value in ERROR_TYPE_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {ERROR_TYPE_VALUES!r}")
