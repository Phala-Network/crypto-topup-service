"""Total outbound I/O budgets for the synchronous reference-product handlers."""

from __future__ import annotations

import asyncio
import time
from contextvars import ContextVar

import httpx

OPERATION_TIMEOUT_SECONDS = 25
operation_deadline: ContextVar[float | None] = ContextVar("operation_deadline", default=None)


class DeadlineTransport(httpx.BaseTransport):
    """Use cancellable async HTTP I/O beneath the SDK's synchronous transport interface.

    HTTPX's phase timeouts alone do not stop a peer that continuously trickles bytes. The
    shared handler deadline covers the entire response, including all pages and SDK calls.
    Each exchange owns its async client because handlers run on separate thread/event loops.
    """

    def handle_request(self, request: httpx.Request) -> httpx.Response:
        request = httpx.Request(
            request.method,
            request.url,
            headers=request.headers,
            content=request.read(),
            extensions=request.extensions,
        )
        deadline = operation_deadline.get()
        if deadline is None:
            deadline = time.monotonic() + OPERATION_TIMEOUT_SECONDS

        async def exchange() -> httpx.Response:
            async with asyncio.timeout(max(0, deadline - time.monotonic())):
                async with httpx.AsyncClient(follow_redirects=False) as client:
                    response = await client.send(request, stream=True)
                    try:
                        content = b"".join([chunk async for chunk in response.aiter_raw()])
                        # Preserve wire headers and let the sync response decode content once.
                        return httpx.Response(
                            response.status_code,
                            headers=response.headers,
                            content=content,
                            extensions=response.extensions,
                        )
                    finally:
                        await response.aclose()

        try:
            return asyncio.run(exchange())
        except TimeoutError as error:
            raise httpx.TimeoutException(
                "product operation deadline exceeded", request=request
            ) from error
