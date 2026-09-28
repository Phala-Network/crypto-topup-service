from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="CreateTreasuryRequest")


@_attrs_define
class CreateTreasuryRequest:
    """`POST /v1/treasuries` body.

    Example:
        {'chain_id': 1, 'message': 'pay-api.phala.com wants you to sign in with your Ethereum
            account:\\n0x936c1991f8dA9a919fa11b557a3514719f5A4504\\n\\nSet this address as the test mode treasury of
            acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 on Phala Pay.\\n\\nURI: https://pay-api.phala.com\\nVersion: 1\\nChain ID:
            1\\nNonce: Kq3nV8xZt2mP6wRa\\nIssued At: 2026-09-28T12:00:00Z\\nExpiration Time: 2026-09-28T12:10:00Z',
            'signature': '0x5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e
            5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e1b'}

    Attributes:
        chain_id (int): The treasury's chain, the challenge's.
        message (str): The challenge's `message`, unchanged.
        signature (str): Hex signature of the message: an EOA's 65-byte `personal_sign` signature, or what a
            deployed contract's `isValidSignature` accepts (for a Safe, the owners' signatures of the
            Safe message, or `0x` after `SignMessageLib` approved it). ERC-6492 signatures are refused.
    """

    chain_id: int
    message: str
    signature: str

    def to_dict(self) -> dict[str, Any]:
        chain_id = self.chain_id

        message = self.message

        signature = self.signature

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "chain_id": chain_id,
                "message": message,
                "signature": signature,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        chain_id = d.pop("chain_id")

        message = d.pop("message")

        signature = d.pop("signature")

        create_treasury_request = cls(
            chain_id=chain_id,
            message=message,
            signature=signature,
        )

        return create_treasury_request
