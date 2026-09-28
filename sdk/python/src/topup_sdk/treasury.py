"""Signing a treasury proof with an EOA (design D10).

`POST /v1/treasuries/challenge` returns an EIP-4361 message naming the treasury, the chain, and
your account; an EOA treasury proves itself with an EIP-191 `personal_sign` signature of it,
which the service checks with `ecrecover`. A Safe treasury's owners sign the same message as a
Safe message instead (docs/integration.md, "Set a Safe as treasury"); this module is for EOAs.

The signature needs a secp256k1 signer: install the `eoa` extra, `pip install
"phala-pay[eoa]"`, which adds `eth-account`, or sign the message with any wallet and submit the
hex signature yourself.
"""

from __future__ import annotations

from typing import Any

from .addresses import same_address, to_checksum_address


def sign_treasury_challenge(message: str, private_key: str | bytes, *, address: str) -> str:
    """Signs a treasury challenge's `message` with `private_key` (EIP-191 `personal_sign`) and
    returns the 65-byte signature as hex, for `POST /v1/treasuries`.

    Fails closed: raises `ValueError` unless the key is `address`'s, the treasury the message
    names, so a wrong key is caught before anything is sent. Keep the key out of logs and
    source; load it from a secret manager or a hardware wallet's signer instead where you can."""
    account, encode_defunct = _eth_account()
    signer = account.from_key(private_key)
    if not same_address(signer.address, address):
        raise ValueError("the private key is not the treasury address's")
    # EIP-4361 puts the EIP-55 address alone on the message's second line.
    if f"\n{to_checksum_address(address)}\n" not in message:
        raise ValueError("the message does not name the treasury address")
    signed = signer.sign_message(encode_defunct(text=message))
    return "0x" + bytes(signed.signature).hex()


def _eth_account() -> tuple[Any, Any]:
    try:
        # An optional dependency: imported only when an EOA signs.
        from eth_account import Account  # noqa: PLC0415
        from eth_account.messages import encode_defunct  # noqa: PLC0415
    except ImportError as error:  # pragma: no cover - depends on the installed extra
        raise ImportError(
            'signing a treasury proof needs eth-account: pip install "phala-pay[eoa]"'
        ) from error
    return Account, encode_defunct
