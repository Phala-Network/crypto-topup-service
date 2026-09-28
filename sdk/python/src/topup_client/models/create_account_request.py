from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.contact import Contact
    from ..models.due_diligence import DueDiligence


T = TypeVar("T", bound="CreateAccountRequest")


@_attrs_define
class CreateAccountRequest:
    """`POST /v1/admin/accounts` body. Accounts are created only by the operator (design D8).

    Attributes:
        contact (Contact): The merchant's contact recorded at onboarding (design D8): the operator's channel for the key
            hand-over, recovery, incidents, and restores, and the only personal data kept.
        due_diligence (DueDiligence): The record of the operator's offline due diligence (design D8): a reference to it,
            when, and
            by whom.
        name (str): Display name, 1 to 200 characters.
        reason (str): Why the account is created, 1 to 1024 bytes.
        charges_enabled (bool | Unset): Whether the account may use live mode (design D12). Default `false`.
    """

    contact: Contact
    due_diligence: DueDiligence
    name: str
    reason: str
    charges_enabled: bool | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.contact import Contact  # noqa: PLC0415
        from ..models.due_diligence import DueDiligence  # noqa: PLC0415

        contact = self.contact.to_dict()

        due_diligence = self.due_diligence.to_dict()

        name = self.name

        reason = self.reason

        charges_enabled = self.charges_enabled

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "contact": contact,
                "due_diligence": due_diligence,
                "name": name,
                "reason": reason,
            }
        )
        if charges_enabled is not UNSET:
            field_dict["charges_enabled"] = charges_enabled

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.contact import Contact  # noqa: PLC0415
        from ..models.due_diligence import DueDiligence  # noqa: PLC0415

        d = dict(src_dict)
        contact = Contact.from_dict(d.pop("contact"))

        due_diligence = DueDiligence.from_dict(d.pop("due_diligence"))

        name = d.pop("name")

        reason = d.pop("reason")

        charges_enabled = d.pop("charges_enabled", UNSET)

        create_account_request = cls(
            contact=contact,
            due_diligence=due_diligence,
            name=name,
            reason=reason,
            charges_enabled=charges_enabled,
        )

        return create_account_request
