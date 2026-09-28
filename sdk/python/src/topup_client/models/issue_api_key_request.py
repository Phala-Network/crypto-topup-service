from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset


T = TypeVar("T", bound="IssueApiKeyRequest")


@_attrs_define
class IssueApiKeyRequest:
    """`POST /v1/admin/accounts/{account}/api_keys` body: a recovery key (design D7).

    Attributes:
        livemode (bool): The key's mode; `true` needs `charges_enabled`.
        reason (str): Why, 1 to 1024 bytes: how the request was verified with the recorded contact.
        name (str | Unset): The key's label, at most 200 characters.
        revoke_existing (bool | Unset): Revokes every key of the mode first, for a leak the merchant cannot win by
            rolling.
    """

    livemode: bool
    reason: str
    name: str | Unset = UNSET
    revoke_existing: bool | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        livemode = self.livemode

        reason = self.reason

        name = self.name

        revoke_existing = self.revoke_existing

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "livemode": livemode,
                "reason": reason,
            }
        )
        if name is not UNSET:
            field_dict["name"] = name
        if revoke_existing is not UNSET:
            field_dict["revoke_existing"] = revoke_existing

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        livemode = d.pop("livemode")

        reason = d.pop("reason")

        name = d.pop("name", UNSET)

        revoke_existing = d.pop("revoke_existing", UNSET)

        issue_api_key_request = cls(
            livemode=livemode,
            reason=reason,
            name=name,
            revoke_existing=revoke_existing,
        )

        return issue_api_key_request
