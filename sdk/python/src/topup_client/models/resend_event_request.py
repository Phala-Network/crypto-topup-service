from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset


T = TypeVar("T", bound="ResendEventRequest")


@_attrs_define
class ResendEventRequest:
    """`POST /v1/events/{id}/resend` body.

    Attributes:
        webhook_endpoint (str): The enabled endpoint to deliver the event to again, `we_…`.
    """

    webhook_endpoint: str

    def to_dict(self) -> dict[str, Any]:
        webhook_endpoint = self.webhook_endpoint

        field_dict: dict[str, Any] = {}

        field_dict.update(
            {
                "webhook_endpoint": webhook_endpoint,
            }
        )

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        d = dict(src_dict)
        webhook_endpoint = d.pop("webhook_endpoint")

        resend_event_request = cls(
            webhook_endpoint=webhook_endpoint,
        )

        return resend_event_request
