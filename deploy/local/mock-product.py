#!/usr/bin/env python3
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlparse


DEPOSIT_ID = "44444444-4444-4444-4444-444444444444"
KEY = f"deposit:{DEPOSIT_ID}"
PAYLOAD = {
    "version": 1,
    "idempotency_key": KEY,
    "account_id": "restore-drill-account",
    "unit": "USD",
    "amount_minor": "275",
    "source": "crypto_deposit",
    "evidence": {
        "chain_id": 1,
        "asset_contract": "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "route": "restore-drill",
        "route_version": 1,
        "tx_hash": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "log_index": 0,
        "to": "0xdddddddddddddddddddddddddddddddddddddddd",
        "amount_atomic": "1000",
        "price_scaled": "27500000",
        "price_scale": 8,
        "valuation_at": "2026-09-22T00:00:01Z",
        "lock_ref": None,
    },
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path = unquote(urlparse(self.path).path)
        if path == "/health":
            self.send_response(200)
            self.end_headers()
            return
        if path != f"/settlements/{KEY}":
            self.send_error(404)
            return
        required = ["content-digest", "idempotency-key", "signature-input", "signature"]
        if any(not self.headers.get(name) for name in required):
            self.send_error(400, "signed settlement headers required")
            return
        if self.headers["idempotency-key"] != f'"{KEY}"':
            self.send_error(400, "wrong idempotency key")
            return
        signature_input = self.headers["signature-input"]
        if 'keyid="settlement/v1"' not in signature_input or '"@method"' not in signature_input:
            self.send_error(400, "wrong signature profile")
            return
        body = json.dumps(
            {
                "status": "accepted",
                "destination_tx_id": "restore-drill-credit",
                "payload": PAYLOAD,
            },
            separators=(",", ":"),
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        return


ThreadingHTTPServer(("0.0.0.0", 8081), Handler).serve_forever()
