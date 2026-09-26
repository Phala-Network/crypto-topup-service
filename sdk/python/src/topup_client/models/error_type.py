from enum import StrEnum


class ErrorType(StrEnum):
    API_ERROR = "api_error"
    IDEMPOTENCY_ERROR = "idempotency_error"
    INVALID_REQUEST_ERROR = "invalid_request_error"

    def __str__(self) -> str:
        return str(self.value)
