"""SDK exception types."""

from __future__ import annotations


class TopupError(Exception):
    """Base class for SDK failures."""


class SignatureError(TopupError):
    """An inbound request or webhook failed signature verification."""


class AttestationError(TopupError):
    """An attestation response does not bind the keys it reports."""


class AddressMismatchError(TopupError):
    """The service returned an address the product cannot derive from its pinned forwarder."""


class ApiError(TopupError):
    """The service answered with a documented error object or an unexpected status.

    `error_type` is `invalid_request_error`, `idempotency_error`, or `api_error`; `param` names the
    request parameter the error is about, when there is one; `doc_url` documents `code`;
    `request_id` is the response's `Request-Id`, to quote to support; `retry_after` is the
    seconds a `429` asked to wait (`Retry-After`).
    """

    def __init__(
        self,
        status_code: int,
        code: str,
        message: str,
        *,
        error_type: str | None = None,
        param: str | None = None,
        doc_url: str | None = None,
        request_id: str | None = None,
        retry_after: float | None = None,
    ) -> None:
        suffix = f" (request {request_id})" if request_id else ""
        super().__init__(f"{status_code} {code}: {message}{suffix}")
        self.status_code = status_code
        self.code = code
        self.message = message
        self.error_type = error_type
        self.param = param
        self.doc_url = doc_url
        self.request_id = request_id
        self.retry_after = retry_after
