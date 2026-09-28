"""Sweeping without Phala Pay: the factory's `flush` call and Safe Transaction Builder batches.

The merchant sweeps its forwarders with its own wallet or Safe and pays the gas (design D4).
`flush_transaction` encodes `ForwarderFactory.flush(treasury, salts, token)` offline from the
exported `(factory, salt, treasury)` of each forwarder (`GET /v1/forwarders`), so funds stay
sweepable even if the service is gone; `flush_transactions` groups the forwarders
`GET /v1/forwarders?sweepable=<token>` lists into one call per factory and treasury.
`safe_batch` writes those calls as a Safe{Wallet} Transaction Builder batch file for a Safe
treasury's owners to import, sign, and execute.

The batch file follows the Transaction Builder's `BatchFile` type
(safe-global/safe-react-apps, `apps/tx-builder/src/typings/models.ts` at commit
e8cccfb9a1042fa2954087988bae59c3b8c81780):

    interface BatchFile {
      version: string; chainId: string; createdAt: number;
      meta: { txBuilderVersion?: string; checksum?: string; createdFromSafeAddress?: string;
              createdFromOwnerAddress?: string; name: string; description?: string };
      transactions: { to: string; value: string; data?: string;
                      contractMethod?: ContractMethod;
                      contractInputsValues?: { [key: string]: string } }[];
    }

`meta.checksum` is the app's own (`apps/tx-builder/src/lib/checksum.ts`): the Keccak-256 of a
key-sorted serialization of the file with `meta.name` set to `null` and no checksum, so the
Transaction Builder imports the file without its "modified since it was generated" warning.
"""

from __future__ import annotations

import json
import time
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any, Protocol

from .addresses import keccak256, to_checksum_address

FLUSH_SIGNATURE = "flush(address,bytes32[],address)"
_FLUSH_SELECTOR = keccak256(FLUSH_SIGNATURE.encode("ascii"))[:4]

# The Transaction Builder's batch file format version.
BATCH_FILE_VERSION = "1.0"


class Call(Protocol):
    """A contract call with `to`, `data`, and a decimal `value`."""

    @property
    def to(self) -> str: ...

    @property
    def data(self) -> str: ...

    @property
    def value(self) -> str: ...


def flush_transaction(
    factory: str, treasury: str, salts: Sequence[str | bytes], token: str
) -> dict[str, str]:
    """Encodes `factory.flush(treasury, salts, token)` offline: `{"to", "data", "value"}` with
    `value` `"0"`. Send it from any account (it pays the gas); the factory moves every listed
    forwarder's whole balance of `token` to `treasury`, the only address a forwarder can pay.

    `salts` are the forwarders' 32-byte salts from `GET /v1/forwarders`, each of a forwarder
    whose `treasury` is this one: a salt issued over another treasury names another forwarder,
    which the call skips as empty."""
    if not salts:
        raise ValueError("flush needs at least one salt")
    encoded_salts = [_bytes32(salt) for salt in salts]
    head = _address_word(treasury) + (3 * 32).to_bytes(32, "big") + _address_word(token)
    tail = len(encoded_salts).to_bytes(32, "big") + b"".join(encoded_salts)
    return {
        "to": to_checksum_address(factory),
        "data": "0x" + (_FLUSH_SELECTOR + head + tail).hex(),
        "value": "0",
    }


class ForwarderLike(Protocol):
    """A forwarder as `GET /v1/forwarders` returns it."""

    @property
    def chain_id(self) -> int: ...

    @property
    def factory(self) -> str: ...

    @property
    def treasury(self) -> str: ...

    @property
    def salt(self) -> str: ...


MAX_SALTS_PER_FLUSH = 200
"""Forwarders per `flush` call, bounding its calldata and gas."""


def flush_transactions(
    forwarders: Iterable[ForwarderLike], token: str, *, max_salts: int = MAX_SALTS_PER_FLUSH
) -> list[dict[str, str]]:
    """Groups forwarders of one chain, such as `GET /v1/forwarders?sweepable=<token>` lists,
    into `flush_transaction` calls: one per factory and treasury, of at most `max_salts`
    forwarders each."""
    groups: dict[tuple[str, str], list[str]] = {}
    chains = set()
    for forwarder in forwarders:
        chains.add(forwarder.chain_id)
        key = (to_checksum_address(forwarder.factory), to_checksum_address(forwarder.treasury))
        groups.setdefault(key, []).append(forwarder.salt)
    if len(chains) > 1:
        raise ValueError("a batch of flush calls is for one chain")
    return [
        flush_transaction(factory, treasury, salts[start : start + max_salts], token)
        for (factory, treasury), salts in groups.items()
        for start in range(0, len(salts), max_salts)
    ]


def safe_batch(
    chain_id: int,
    safe: str,
    calls: Iterable[Call | Mapping[str, str]],
    *,
    name: str = "Phala Pay sweep",
    description: str = "",
    created_at_ms: int | None = None,
) -> dict[str, Any]:
    """Returns a Safe Transaction Builder batch file of `calls` for the Safe `safe` on
    `chain_id`, with its `meta.checksum`. Write it with `write_safe_batch`; a Safe owner imports
    it in Safe{Wallet} > Apps > Transaction Builder, the owners sign it, and one executes it.

    `calls` are `flush_transaction` results or any `{to, data, value}` mappings; `value` is a
    decimal string, as the app requires."""
    transactions = []
    for call in calls:
        to, data, value = (
            (call["to"], call["data"], call.get("value", "0"))
            if isinstance(call, Mapping)
            else (call.to, call.data, call.value)
        )
        if not isinstance(value, str) or not value.isdigit():
            raise ValueError("a call's value must be a decimal string")
        if not data.startswith("0x"):
            raise ValueError("a call's data must be 0x-prefixed hex")
        bytes.fromhex(data[2:])
        transactions.append({"to": to_checksum_address(to), "value": value, "data": data})
    if not transactions:
        raise ValueError("a batch needs at least one call")
    if not name:
        raise ValueError("a batch needs a name")
    batch: dict[str, Any] = {
        "version": BATCH_FILE_VERSION,
        "chainId": str(chain_id),
        "createdAt": int(time.time() * 1000) if created_at_ms is None else created_at_ms,
        "meta": {
            "name": name,
            "description": description,
            "createdFromSafeAddress": to_checksum_address(safe),
        },
        "transactions": transactions,
    }
    batch["meta"]["checksum"] = batch_checksum(batch)
    return batch


def write_safe_batch(path: str | Path, batch: Mapping[str, Any]) -> Path:
    """Writes a batch file as JSON, as the Transaction Builder's own download does."""
    target = Path(path)
    target.write_text(json.dumps(batch, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return target


def batch_checksum(batch: Mapping[str, Any]) -> str:
    """The Transaction Builder's `meta.checksum` of a batch file: Keccak-256 of the app's
    `serializeJSONObject` of the file with `meta.name` set to `null`, over the file as it is
    without its checksum (the app's `validateChecksum` deletes it before recomputing)."""
    meta = {key: value for key, value in batch["meta"].items() if key != "checksum"}
    subject = {**batch, "meta": {**meta, "name": None}}
    return "0x" + keccak256(_serialize(subject).encode("utf-8")).hex()


def _serialize(value: Any) -> str:
    """The app's `serializeJSONObject`: arrays element-wise; objects as their sorted key array
    followed by each value and a comma; anything else as `JSON.stringify` writes it."""
    if isinstance(value, list):
        return "[" + ",".join(_serialize(element) for element in value) + "]"
    if isinstance(value, dict):
        keys = sorted(value)
        return "{" + _json(keys) + "".join(_serialize(value[key]) + "," for key in keys) + "}"
    return _json(value)


def _json(value: Any) -> str:
    if isinstance(value, float):
        raise TypeError("batch files hold no floating-point numbers")
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def _address_word(address: str) -> bytes:
    return bytes(12) + bytes.fromhex(to_checksum_address(address)[2:])


def _bytes32(salt: str | bytes) -> bytes:
    raw = salt if isinstance(salt, bytes) else bytes.fromhex(salt.removeprefix("0x"))
    if len(raw) != 32:
        raise ValueError("a salt is 32 bytes")
    return raw
