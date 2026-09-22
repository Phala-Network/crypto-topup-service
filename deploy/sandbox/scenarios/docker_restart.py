"""Restarts one container through the Docker Engine API on /var/run/docker.sock.

The local sandbox runs scenarios in a container without the Docker CLI; this is its
`restart_command`. Usage: python docker_restart.py CONTAINER
"""

from __future__ import annotations

import http.client
import socket
import sys
from urllib.parse import quote

DOCKER_SOCKET = "/var/run/docker.sock"


class _UnixConnection(http.client.HTTPConnection):
    def connect(self) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(DOCKER_SOCKET)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: docker_restart.py CONTAINER", file=sys.stderr)
        return 2
    connection = _UnixConnection("localhost", timeout=60)
    connection.request("POST", f"/containers/{quote(sys.argv[1], safe='')}/restart?t=10")
    status = connection.getresponse().status
    connection.close()
    if status != 204:
        print(f"docker restart failed with HTTP {status}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
