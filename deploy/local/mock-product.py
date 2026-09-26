#!/usr/bin/env python3
"""Local stand-in for a product's webhook receiver: acknowledges every delivery.

The local stack and the restore drill register it as the product's webhook URL, and the drill
uses its container as a client on the compose network. It verifies nothing and credits nothing.
"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if urlparse(self.path).path != "/health":
            self.send_error(404)
            return
        self.send_response(200)
        self.end_headers()

    def do_POST(self):
        if urlparse(self.path).path != "/webhooks":
            self.send_error(404)
            return
        self.rfile.read(int(self.headers.get("content-length") or 0))
        self.send_response(204)
        self.end_headers()

    def log_message(self, format, *args):
        return


ThreadingHTTPServer(("0.0.0.0", 8081), Handler).serve_forever()
