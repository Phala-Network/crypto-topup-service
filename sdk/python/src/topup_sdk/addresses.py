"""Deterministic deposit identities and CREATE2 forwarder addresses.

Products recompute every address the service hands out from the returned salt inputs, and
recompute every deposit id from its chain evidence, so neither has to be trusted blindly.
"""

from __future__ import annotations

import uuid

from Crypto.Hash import keccak

DEPOSIT_NAMESPACE = uuid.UUID("d55bab89-f656-5796-a6a2-bddfa1dd9631")

_CLONE_CREATION_PREFIX = bytes.fromhex("3d602d80600a3d3981f3")
_CLONE_RUNTIME_PREFIX = bytes.fromhex("363d3d373d3d3d363d73")
_CLONE_RUNTIME_SUFFIX = bytes.fromhex("5af43d82803e903d91602b57fd5bf3")


def keccak256(data: bytes) -> bytes:
    """Returns the Ethereum Keccak-256 digest."""
    return bytes(keccak.new(digest_bits=256, data=data).digest())


def deposit_id(chain_id: int, tx_hash: str, log_index: int) -> uuid.UUID:
    """Returns UUIDv5(NS, `{chain_id}:{lowercase tx_hash}:{log_index}`)."""
    tx_hash = tx_hash.lower()
    if len(_hex_bytes(tx_hash)) != 32:
        raise ValueError("tx_hash must be 32 bytes")
    return uuid.uuid5(DEPOSIT_NAMESPACE, f"{chain_id}:{tx_hash}:{log_index}")


def persistent_salt(product_slug: str, external_id: str, version: int) -> bytes:
    """keccak256(abi.encode(product_slug, external_id, uint256 version))."""
    return keccak256(_abi_encode(product_slug, external_id, version))


def lock_salt(product_slug: str, external_id: str, lock_ref: str) -> bytes:
    """keccak256(abi.encode(product_slug, external_id, "lock", lock_ref))."""
    return keccak256(_abi_encode(product_slug, external_id, "lock", lock_ref))


def forwarder_address(factory: str, implementation: str, salt: bytes) -> str:
    """Predicts the OpenZeppelin EIP-1167 clone address deployed by `factory` with `salt`."""
    if len(salt) != 32:
        raise ValueError("salt must be 32 bytes")
    init_code = (
        _CLONE_CREATION_PREFIX
        + _CLONE_RUNTIME_PREFIX
        + _address_bytes(implementation)
        + _CLONE_RUNTIME_SUFFIX
    )
    preimage = b"\xff" + _address_bytes(factory) + salt + keccak256(init_code)
    return to_checksum_address(keccak256(preimage)[12:])


def to_checksum_address(address: bytes | str) -> str:
    """Formats a 20-byte address with its EIP-55 checksum."""
    raw = address if isinstance(address, bytes) else _address_bytes(address)
    if len(raw) != 20:
        raise ValueError("address must be 20 bytes")
    lower = raw.hex()
    digest = keccak256(lower.encode("ascii")).hex()
    return "0x" + "".join(
        char.upper() if int(nibble, 16) >= 8 else char
        for char, nibble in zip(lower, digest[: len(lower)], strict=True)
    )


def same_address(left: str, right: str) -> bool:
    """Compares two EVM addresses byte-wise."""
    return _address_bytes(left) == _address_bytes(right)


def _address_bytes(address: str) -> bytes:
    raw = _hex_bytes(address)
    if len(raw) != 20:
        raise ValueError("address must be 20 bytes")
    return raw


def _hex_bytes(value: str) -> bytes:
    if not value.startswith(("0x", "0X")):
        raise ValueError("expected 0x-prefixed hexadecimal")
    return bytes.fromhex(value[2:])


def _abi_encode(*fields: str | int) -> bytes:
    """ABI-encodes a tuple of `string` and `uint256` fields, as `abi.encode` does."""
    head = b""
    tail = b""
    offset = 32 * len(fields)
    for field in fields:
        if isinstance(field, int):
            if not 0 <= field < 2**256:
                raise ValueError("uint256 out of range")
            head += field.to_bytes(32, "big")
            continue
        encoded = field.encode("utf-8")
        head += (offset + len(tail)).to_bytes(32, "big")
        tail += len(encoded).to_bytes(32, "big") + encoded + b"\x00" * (-len(encoded) % 32)
    return head + tail
