from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.confirmation_policy import ConfirmationPolicy


T = TypeVar("T", bound="UpdateAccountObjectRequest")


@_attrs_define
class UpdateAccountObjectRequest:
    """`POST /v1/account` body; parameters not sent are left unchanged.

    Attributes:
        confirmation_policies (list[ConfirmationPolicy] | None | Unset): The confirmations to require, per chain; chains
            not listed keep theirs.
    """

    confirmation_policies: list[ConfirmationPolicy] | None | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.confirmation_policy import ConfirmationPolicy  # noqa: PLC0415

        confirmation_policies: list[dict[str, Any]] | None | Unset
        if isinstance(self.confirmation_policies, Unset):
            confirmation_policies = UNSET
        elif isinstance(self.confirmation_policies, list):
            confirmation_policies = []
            for confirmation_policies_type_0_item_data in self.confirmation_policies:
                confirmation_policies_type_0_item = confirmation_policies_type_0_item_data.to_dict()
                confirmation_policies.append(confirmation_policies_type_0_item)

        else:
            confirmation_policies = self.confirmation_policies

        field_dict: dict[str, Any] = {}

        field_dict.update({})
        if confirmation_policies is not UNSET:
            field_dict["confirmation_policies"] = confirmation_policies

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.confirmation_policy import ConfirmationPolicy  # noqa: PLC0415

        d = dict(src_dict)

        def _parse_confirmation_policies(data: object) -> list[ConfirmationPolicy] | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, list):
                    raise TypeError()
                confirmation_policies_type_0 = []
                _confirmation_policies_type_0 = data
                for confirmation_policies_type_0_item_data in _confirmation_policies_type_0:
                    confirmation_policies_type_0_item = ConfirmationPolicy.from_dict(
                        confirmation_policies_type_0_item_data
                    )

                    confirmation_policies_type_0.append(confirmation_policies_type_0_item)

                return confirmation_policies_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(list[ConfirmationPolicy] | None | Unset, data)

        confirmation_policies = _parse_confirmation_policies(d.pop("confirmation_policies", UNSET))

        update_account_object_request = cls(
            confirmation_policies=confirmation_policies,
        )

        return update_account_object_request
