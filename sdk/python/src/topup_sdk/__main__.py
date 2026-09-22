"""Command-line helpers for integrators.

`keygen` creates a product signing key for credential issuance: the seed file stays with the
integrator, and only the printed public key and key id are sent to the service operator.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import sys
from pathlib import Path

from .signing import RequestSigner


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="topup-sdk")
    commands = parser.add_subparsers(dest="command", required=True)
    keygen = commands.add_parser("keygen", help="create an ed25519 product signing key")
    keygen.add_argument("--keyid", required=True, help="key identifier, for example acme/v1")
    keygen.add_argument("--seed-out", required=True, type=Path, help="new file for the seed")
    public = commands.add_parser("public-key", help="print the public key of a seed file")
    public.add_argument("--keyid", required=True)
    public.add_argument("--seed-file", required=True, type=Path)
    args = parser.parse_args(argv)

    if args.command == "keygen":
        seed = secrets.token_bytes(32)
        try:
            descriptor = os.open(args.seed_out, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            print(f"refusing to overwrite {args.seed_out}", file=sys.stderr)
            return 1
        with os.fdopen(descriptor, "w", encoding="ascii") as handle:
            handle.write(seed.hex() + "\n")
        signer = RequestSigner.from_seed(args.keyid, seed)
    else:
        signer = RequestSigner.from_seed_file(args.keyid, args.seed_file)
    print(json.dumps({"keyid": signer.keyid, "public_key": signer.public_key_base64()}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
