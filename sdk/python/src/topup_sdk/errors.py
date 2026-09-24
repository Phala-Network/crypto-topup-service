"""SDK exception types."""

from __future__ import annotations


class TopupError(Exception):
    """Base class for SDK failures."""


class SignatureError(TopupError):
    """An inbound request or webhook failed signature verification."""


class AttestationError(TopupError):
    """An attestation response does not bind the keys it reports."""


class ApiError(TopupError):
    """The service answered with a documented error envelope or an unexpected status."""

    def __init__(self, status_code: int, code: str, message: str) -> None:
        super().__init__(f"{status_code} {code}: {message}")
        self.status_code = status_code
        self.code = code
        self.message = message
