from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.metadata_clear import check_metadata_clear
from ..models.metadata_clear import MetadataClear
from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.metadata_param_type_0 import MetadataParamType0


T = TypeVar("T", bound="CreateDepositAddressRequest")


@_attrs_define
class CreateDepositAddressRequest:
    """`POST /v1/deposit_addresses` body.

    Example:
        {'client_reference_id': 'team-42', 'metadata': {'plan': 'pro'}}

    Attributes:
        client_reference_id (str): Your identifier of the customer, 1 to 200 characters; the customer is created on
            first use.
        metadata (MetadataClear | MetadataParamType0 | Unset): A `metadata` parameter: an object of string values, where
            `""` unsets the key, or `""` to
            unset every key.
    """

    client_reference_id: str
    metadata: MetadataClear | MetadataParamType0 | Unset = UNSET

    def to_dict(self) -> dict[str, Any]:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        client_reference_id = self.client_reference_id

        metadata: dict[str, Any] | str | Unset
        if isinstance(self.metadata, Unset):
            metadata = UNSET
        elif isinstance(self.metadata, MetadataParamType0):
            metadata = self.metadata.to_dict()
        else:
            metadata = self.metadata

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "client_reference_id": client_reference_id,
            }
        )
        if metadata is not UNSET:
            field_dict["metadata"] = metadata

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.metadata_param_type_0 import MetadataParamType0  # noqa: PLC0415

        d = dict(src_dict)
        client_reference_id = d.pop("client_reference_id")

        def _parse_metadata(data: object) -> MetadataClear | MetadataParamType0 | Unset:
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_metadata_param_type_0 = MetadataParamType0.from_dict(data)

                return componentsschemas_metadata_param_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            if not isinstance(data, str):
                raise TypeError()
            componentsschemas_metadata_param_type_1 = check_metadata_clear(data)

            return componentsschemas_metadata_param_type_1

        metadata = _parse_metadata(d.pop("metadata", UNSET))

        create_deposit_address_request = cls(
            client_reference_id=client_reference_id,
            metadata=metadata,
        )

        return create_deposit_address_request
