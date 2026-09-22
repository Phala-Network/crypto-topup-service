"""RFC 9421 interoperability vectors produced by the Python signer.

The Rust verifier test in `crates/topup/src/api/auth.rs` checks every vector in the committed
file; `tests/test_signing.py` checks that the Python signer still reproduces it byte for byte.
Regenerate after an intentional profile change with `uv run python -m tests.vectors`.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import httpx

from topup_sdk.signing import RequestSigner, target_uri

VECTORS_PATH = (
    Path(__file__).resolve().parents[3] / "crates/topup/tests/fixtures/rfc9421-python-signer.json"
)
SEED = bytes(range(32))
KEYID = "sdk-vector/v1"
CREATED = 1_790_000_000
BASE_URL = "http://127.0.0.1:18080"

CASES: list[dict[str, Any]] = [
    {
        "name": "register_account",
        "method": "POST",
        "path": "/v1/products/sdk-vector/accounts",
        "params": {},
        "body": {"external_id": "workspace-42"},
        "idempotency_key": None,
        "include_alg": True,
    },
    {
        "name": "list_deposits_with_query",
        "method": "GET",
        "path": "/v1/products/sdk-vector/accounts/%E5%AE%A2%E6%88%B7%2042/deposits",
        "params": {"state": "credited", "from": "2026-09-01T00:00:00+00:00"},
        "body": None,
        "idempotency_key": None,
        "include_alg": True,
    },
    {
        "name": "idempotency_key_covered",
        "method": "POST",
        "path": "/v1/products/sdk-vector/accounts/workspace-42/rate-locks",
        "params": {},
        "body": {"amount_minor": "2500", "product_lock_ref": "checkout-1"},
        "idempotency_key": '"checkout-1"',
        "include_alg": True,
    },
    {
        "name": "without_alg_parameter",
        "method": "DELETE",
        "path": "/v1/products/sdk-vector/accounts/workspace-42/rate-locks/checkout-1",
        "params": {},
        "body": None,
        "idempotency_key": None,
        "include_alg": False,
    },
]


def build_vectors() -> dict[str, Any]:
    """Signs every case exactly as `TopupClient` would send it over httpx."""
    vectors = []
    for case in CASES:
        signer = RequestSigner.from_seed(
            KEYID, SEED, include_alg=case["include_alg"], clock=lambda: CREATED
        )
        headers = {}
        if case["idempotency_key"] is not None:
            headers["idempotency-key"] = case["idempotency_key"]
        request = httpx.Request(
            case["method"],
            httpx.URL(BASE_URL + case["path"], params=case["params"]),
            json=case["body"],
            headers=headers,
        )
        request.headers.update(
            signer.sign(
                request.method,
                target_uri(request),
                request.content,
                idempotency_key=request.headers.get("idempotency-key"),
            )
        )
        vectors.append(
            {
                "name": case["name"],
                "method": request.method,
                "target": request.url.raw_path.decode("ascii"),
                "target_uri": target_uri(request),
                "headers": {
                    name: request.headers[name]
                    for name in (
                        "host",
                        "content-type",
                        "idempotency-key",
                        "content-digest",
                        "signature-input",
                        "signature",
                    )
                    if name in request.headers
                },
                "body": request.content.decode("utf-8"),
            }
        )
    return {
        "description": (
            "RFC 9421 requests signed by sdk/python (topup_sdk.signing). Regenerate with "
            "`uv run python -m tests.vectors` from sdk/python."
        ),
        "keyid": KEYID,
        "seed_hex": SEED.hex(),
        "public_key": RequestSigner.from_seed(KEYID, SEED).public_key_base64(),
        "created": CREATED,
        "vectors": vectors,
    }


def render() -> str:
    return json.dumps(build_vectors(), indent=2, ensure_ascii=False) + "\n"


if __name__ == "__main__":
    VECTORS_PATH.write_text(render(), encoding="utf-8")
    print(f"wrote {VECTORS_PATH}")
