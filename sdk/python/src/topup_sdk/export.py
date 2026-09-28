"""`export_account`: the account's data as JSON files (design §13).

The list endpoints are the export (GDPR Art. 20, a "structured, commonly used and machine-readable
format"): this pages through each and writes one file per resource. `forwarders.json` carries
every forwarder's `(factory, salt, treasury)`, so funds stay recomputable and sweepable with
`flush_transaction` even without Phala Pay.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any, Protocol

from .client import TopupClient


class _Serializable(Protocol):
    def to_dict(self) -> dict[str, Any]: ...


def export_account(client: TopupClient, directory: str | Path) -> dict[str, int]:
    """Writes the key's account and mode to `directory` (created if needed), one JSON file per
    resource, and returns how many objects each holds. Secrets are never included: API keys are
    listed without them, and no webhook signing key leaves the service."""
    target = Path(directory)
    target.mkdir(parents=True, exist_ok=True)
    resources: dict[str, Callable[[], Iterable[_Serializable]]] = {
        "account": lambda: [client.get_account()],
        "config": lambda: [client.get_config()],
        "balance": lambda: [client.get_balance()],
        "quotes": client.list_quotes,
        "deposits": client.list_deposits,
        "refunds": client.list_refunds,
        "deposit_addresses": client.list_deposit_addresses,
        "forwarders": client.list_forwarders,
        "sweeps": client.list_sweeps,
        "treasuries": client.list_treasuries,
        "webhook_endpoints": client.list_webhook_endpoints,
        "api_keys": client.list_api_keys,
        "events": client.list_events,
    }
    counts = {}
    for name, fetch in resources.items():
        objects = [item.to_dict() for item in fetch()]
        (target / f"{name}.json").write_text(
            json.dumps(objects, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        counts[name] = len(objects)
    return counts
