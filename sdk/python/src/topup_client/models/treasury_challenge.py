from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="TreasuryChallenge")


@_attrs_define
class TreasuryChallenge:
    """An EIP-4361 (Sign-In with Ethereum) message proving a treasury, valid for 10 minutes and
    usable once.

        Attributes:
            address (str): The treasury address, as sent.
            chain_id (int): The treasury's chain.
            expires_at (int): When the message stops being accepted, Unix seconds.
            livemode (bool): The key's mode.
            message (str): The EIP-4361 message to sign, exactly as given: `domain` and `URI` are the API's origin,
                the statement names your account and mode, and `Chain ID` is `chain_id`. An EOA signs it
                with `personal_sign` (EIP-191); a Safe's owners sign it as a Safe message (EIP-1271).
            nonce (str): The message's single-use nonce.
            object_ (str): Always `treasury_challenge`.
    """

    address: str
    chain_id: int
    expires_at: int
    livemode: bool
    message: str
    nonce: str
    object_: str
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        address = self.address

        chain_id = self.chain_id

        expires_at = self.expires_at

        livemode = self.livemode

        message = self.message

        nonce = self.nonce

        object_ = self.object_

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

        object_ = d.pop("object")

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
