"""The ASGI boundary bounds malformed, oversized, stalled and concurrent requests."""

from __future__ import annotations

import asyncio
import socket
import sys
import threading
from collections.abc import AsyncIterator, Iterator
from pathlib import Path
from types import SimpleNamespace
from typing import cast
from unittest.mock import Mock

import httpx
import pytest
from starlette.testclient import TestClient

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from reference_product import server as transport
from reference_product.fulfillment import Answer
from reference_product.server import MAX_BODY_BYTES, ProductServer


@pytest.fixture
def product() -> Iterator[ProductServer]:
    fulfillment = Mock()
    fulfillment.config = SimpleNamespace(
        public_url="http://localhost/topup", listen_host="127.0.0.1", listen_port=0
    )
    fulfillment.handle.return_value = Answer(200, {"received": True})
    product = ProductServer(fulfillment)
    try:
        yield product
    finally:
        product._workers.shutdown(wait=True, cancel_futures=True)


@pytest.mark.parametrize("length", ["-1", "invalid", "1.5", "+2", "9" * 5000])
def test_bad_content_length(product: ProductServer, length: str) -> None:
    with TestClient(product.app) as client:
        response = client.post("/topup/webhooks", content=b"{}", headers={"content-length": length})
    assert response.status_code == 400
    assert response.json() == {"code": "bad_request"}
    cast(Mock, product.fulfillment.handle).assert_not_called()


@pytest.mark.parametrize(
    ("body", "length", "status"),
    [
        (b"{}", "3", 400),
        (b"{}", str(MAX_BODY_BYTES + 1), 413),
        (b"x" * (MAX_BODY_BYTES + 1), None, 413),
    ],
)
def test_body_limits(product: ProductServer, body: bytes, length: str | None, status: int) -> None:
    headers = {} if length is None else {"content-length": length}
    with TestClient(product.app) as client:
        response = client.post("/topup/webhooks", content=iter([body]), headers=headers)
    assert response.status_code == status
    cast(Mock, product.fulfillment.handle).assert_not_called()


def test_preserves_dispatch_and_hides_internal_errors(product: ProductServer) -> None:
    with TestClient(product.app) as client:
        assert client.get("/topup/healthz").json() == {"status": "ok"}
        assert client.post("/topup/webhooks", content=b"{}").json() == {"received": True}
        cast(Mock, product.fulfillment.handle).side_effect = RuntimeError("private detail")
        response = client.post("/topup/webhooks", content=b"{}")
        assert response.status_code == 500
        assert response.json() == {"code": "internal_server_error"}


def test_stalled_body_times_out(product: ProductServer, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(transport, "REQUEST_TIMEOUT_SECONDS", 0.01)

    async def body() -> AsyncIterator[bytes]:
        await asyncio.sleep(1)
        yield b"{}"

    async def run() -> httpx.Response:
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(product.app), base_url="http://test"
        ) as client:
            return await client.post("/topup/webhooks", content=body())

    response = asyncio.run(run())
    assert response.status_code == 408
    assert response.json() == {"code": "request_timeout"}
    cast(Mock, product.fulfillment.handle).assert_not_called()


def test_timed_out_workers_remain_bounded(
    product: ProductServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(transport, "REQUEST_TIMEOUT_SECONDS", 0.02)
    product._capacity = threading.BoundedSemaphore(1)
    done = threading.Event()
    cast(Mock, product.fulfillment.handle).side_effect = lambda *_: (done.wait(2), Answer(200))[1]
    try:
        with TestClient(product.app) as client:
            assert client.post("/topup/webhooks", content=b"{}").status_code == 408
            assert client.post("/topup/webhooks", content=b"{}").status_code == 503
            assert cast(Mock, product.fulfillment.handle).call_count == 1
    finally:
        done.set()


@pytest.mark.parametrize("length", ["-1", "invalid"])
def test_h11_rejects_invalid_wire_framing(product: ProductServer, length: str) -> None:
    with product:
        port = product._server.servers[0].sockets[0].getsockname()[1]
        with socket.create_connection(("127.0.0.1", port), timeout=2) as connection:
            connection.sendall(
                (
                    "POST /topup/webhooks HTTP/1.1\r\nHost: localhost\r\n"
                    f"Content-Length: {length}\r\nConnection: close\r\n\r\n"
                ).encode()
            )
            assert connection.recv(4096).startswith(b"HTTP/1.1 400")
    assert not product._thread.is_alive()
    cast(Mock, product.fulfillment.handle).assert_not_called()
