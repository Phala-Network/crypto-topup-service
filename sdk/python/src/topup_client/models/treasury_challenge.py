from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.treasury_challenge_object import check_treasury_challenge_object
from ..models.treasury_challenge_object import TreasuryChallengeObject
from typing import cast


T = TypeVar("T", bound="TreasuryChallenge")


@_attrs_define
class TreasuryChallenge:
    """An EIP-4361 (Sign-In with Ethereum) message proving a treasury, usable once: valid for 10
    minutes for an EOA, 24 hours for an address that holds code (a Safe).

        Example:
            {'address': '0x936c1991f8da9a919fa11b557a3514719f5a4504', 'chain_id': 1, 'expires_at': 1790554200, 'livemode':
                False, 'message': 'pay-api.phala.com wants you to sign in with your Ethereum
                account:\\n0x936c1991f8dA9a919fa11b557a3514719f5A4504\\n\\nSet this address as the test mode treasury of
                acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 on Phala Pay.\\n\\nURI: https://pay-api.phala.com\\nVersion: 1\\nChain ID:
                1\\nNonce: Kq3nV8xZt2mP6wRa\\nIssued At: 2026-09-28T12:00:00Z\\nExpiration Time: 2026-09-28T12:10:00Z', 'nonce':
                'Kq3nV8xZt2mP6wRa', 'object': 'treasury_challenge'}

        Attributes:
            address (str): The treasury address, as sent.
            chain_id (int): The treasury's chain.
            expires_at (int): When the message stops being accepted, Unix seconds.
            livemode (bool): The key's mode.
            message (str): The EIP-4361 message to sign, exactly as given: `domain` and `URI` are the API's origin,
                the statement names your account and mode, and `Chain ID` is `chain_id`. An EOA signs it
                with `personal_sign` (EIP-191); a Safe's owners sign it as a Safe message (EIP-1271).
            nonce (str): The message's single-use nonce.
            object_ (TreasuryChallengeObject): Always `treasury_challenge`.
    """

    address: str
    chain_id: int
    expires_at: int
    livemode: bool
    message: str
    nonce: str
    object_: TreasuryChallengeObject
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        chain_id = self.chain_id

        expires_at = self.expires_at

        livemode = self.livemode

        message = self.message

        nonce = self.nonce

        object_: str = self.object_

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "chain_id": chain_id,
                "expires_at": expires_at,
                "livemode": livemode,
                "message": message,
                "nonce": nonce,
                "object": object_,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        address = d.pop("address")

        chain_id = d.pop("chain_id")

        expires_at = d.pop("expires_at")

        livemode = d.pop("livemode")

        message = d.pop("message")

        nonce = d.pop("nonce")

        object_ = check_treasury_challenge_object(d.pop("object"))

        treasury_challenge = cls(
            address=address,
            chain_id=chain_id,
            expires_at=expires_at,
            livemode=livemode,
            message=message,
            nonce=nonce,
            object_=object_,
        )

        treasury_challenge.additional_properties = d
        return treasury_challenge

    @property
    def additional_keys(self) -> list[str]:
        return list(self.additional_properties.keys())

    def __getitem__(self, key: str) -> Any:
        return self.additional_properties[key]

    def __setitem__(self, key: str, value: Any) -> None:
        self.additional_properties[key] = value

    def __delitem__(self, key: str) -> None:
        del self.additional_properties[key]

    def __contains__(self, key: str) -> bool:
        return key in self.additional_properties
