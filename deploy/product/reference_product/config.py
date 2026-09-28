"""The product's configuration, shared by the product service and the deposit driver."""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path

from topup_sdk import TopupClient

# The deposit driver signs its account API requests with this key id (see `AccountApi`).
DRIVER_KEYID = "driver/v1"
EVM_ADDRESS = re.compile(r"0x[0-9a-fA-F]{40}")


class MissingProductKeyError(Exception):
    """The product's API key is not configured (in a CVM: not sealed yet)."""


@dataclass(frozen=True)
class ProductConfig:
    """Everything the product needs; see deploy/sandbox/README.md for each field.

    The product's Phala Pay secret key (`ppay_sk_…`) comes from `api_key_file`, or, in a CVM,
    from the sealed environment variable named by `api_key_env`. `product_slug` is the product's
    Phala Pay account id (`acct_…`), the first input of every quote's address. The deposit driver
    needs no key: it calls the product's account API at `public_url`, signed with the driver key
    whose public key is `driver_public_key`.
    """

    service_url: str
    product_slug: str
    route: str
    chain_id: int
    rpc_url: str
    factory: str
    implementation: str
    treasury: str
    token: str
    token_symbol: str
    public_url: str
    listen_host: str = "127.0.0.1"
    listen_port: int = 8089
    api_key_file: str | None = None
    api_key_env: str | None = None
    ledger_path: str = ":memory:"
    driver_public_key: str | None = None
    payer: str | None = None
    payer_account: str | None = None
    unsupported_token: str | None = None
    # The account's webhook public keys (hex) in the API key's mode, current first; unset, the
    # product pins them from `GET /v1/attestation` with its API key.
    webhook_public_keys: list[str] | None = None
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

    def api_key(self) -> str:
        if self.api_key_file is not None:
            return Path(self.api_key_file).read_text(encoding="ascii").strip()
        key = os.environ.get(self.api_key_env or "", "").strip()
        if not key:
            raise MissingProductKeyError("no api_key_file, and api_key_env is unset")
        return key

    def livemode(self) -> bool:
        """The mode of the product's API key, and so of its webhooks."""
        return self.api_key().startswith("ppay_sk_live_")

    def client(self) -> TopupClient:
        # The forwarder is pinned, so every open quote's address is recomputed before it is used.
        return TopupClient(
            self.service_url,
            self.api_key(),
            account=self.product_slug,
            forwarder=(self.factory, self.implementation, self.treasury),
        )
