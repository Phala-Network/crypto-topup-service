"""The product's configuration, shared by the product service and the deposit driver."""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path

from topup_sdk import RequestSigner, TopupClient

SETTLEMENT_KEYID = "settlement/v1"
# The deposit driver signs its account API requests with this key id (see `AccountApi`).
DRIVER_KEYID = "driver/v1"
EVM_ADDRESS = re.compile(r"0x[0-9a-fA-F]{40}")


class MissingProductKeyError(Exception):
    """The product key is not configured (in a CVM: not sealed yet)."""


@dataclass(frozen=True)
class ProductConfig:
    """Everything the product needs; see deploy/sandbox/README.md for each field.

    The product key comes from `product_seed_file`, or, in a CVM, from the sealed environment
    variable named by `product_seed_env` (64 hexadecimal characters, as `topup-sdk keygen`
    writes them). The deposit driver needs neither: it calls the product's account API at
    `public_url`, signed with the driver key whose public key is `driver_public_key`.
    """

    service_url: str
    product_slug: str
    product_keyid: str
    route: str
    chain_id: int
    rpc_url: str
    factory: str
    implementation: str
    token: str
    token_symbol: str
    public_url: str
    listen_host: str = "127.0.0.1"
    listen_port: int = 8089
    product_seed_file: str | None = None
    product_seed_env: str | None = None
    ledger_path: str = ":memory:"
    driver_public_key: str | None = None
    payer: str | None = None
    payer_account: str | None = None
    unsupported_token: str | None = None
    settlement_public_key: str | None = None
    per_deposit_cap_minor: int = 100_000
    per_period_cap_minor: int = 500_000
    period_seconds: int = 24 * 60 * 60
    restart_command: list[str] = field(default_factory=list)
    # The built demo checkout page (reference_product.demo); unset, no page is served.
    demo_dir: str | None = None

    @classmethod
    def load(cls, path: str | Path) -> ProductConfig:
        values = json.loads(Path(path).read_text(encoding="utf-8"))
        return cls(**values)

    def signer(self) -> RequestSigner:
        if self.product_seed_file is not None:
            return RequestSigner.from_seed_file(self.product_keyid, self.product_seed_file)
        seed = os.environ.get(self.product_seed_env or "", "").strip()
        if not seed:
            raise MissingProductKeyError("no product_seed_file, and product_seed_env is unset")
        return RequestSigner.from_seed(self.product_keyid, bytes.fromhex(seed))

    def client(self) -> TopupClient:
        # The forwarder is pinned, so every open quote's address is recomputed before it is used.
        return TopupClient(
            self.service_url, self.signer(), forwarder=(self.factory, self.implementation)
        )
