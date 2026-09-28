"""An EOA treasury's proof: the EIP-191 signature the service recovers with `ecrecover`."""

from __future__ import annotations

import pytest
from eth_account import Account
from eth_account.messages import encode_defunct

from topup_sdk import sign_treasury_challenge

KEY = "0x" + "4c" * 32
ADDRESS = Account.from_key(KEY).address


def _message(address: str) -> str:
    # The shape of the service's EIP-4361 challenge (crates/topup/src/treasuries.rs).
    return (
        "api.test wants you to sign in with your Ethereum account:\n"
        f"{address}\n\n"
        "Set this address as the test mode treasury of acct_0a0a on Phala Pay.\n\n"
        "URI: http://api.test\nVersion: 1\nChain ID: 11155111\nNonce: n0nce\n"
        "Issued At: 2026-09-28T00:00:00Z\nExpiration Time: 2026-09-28T00:10:00Z"
    )


def test_the_signature_recovers_to_the_treasury() -> None:
    message = _message(ADDRESS)
    signature = sign_treasury_challenge(message, KEY, address=ADDRESS.lower())
    assert len(bytes.fromhex(signature[2:])) == 65
    recovered = Account.recover_message(encode_defunct(text=message), signature=signature)
    assert recovered == ADDRESS


def test_a_wrong_key_or_message_is_refused_before_signing() -> None:
    other = Account.create().address
    with pytest.raises(ValueError, match="not the treasury"):
        sign_treasury_challenge(_message(other), KEY, address=other)
    with pytest.raises(ValueError, match="does not name"):
        sign_treasury_challenge(_message(other), KEY, address=ADDRESS)
