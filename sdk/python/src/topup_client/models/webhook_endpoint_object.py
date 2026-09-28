from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..models.webhook_endpoint_object_object import check_webhook_endpoint_object_object
from ..models.webhook_endpoint_object_object import WebhookEndpointObjectObject
from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.delivery_attempt import DeliveryAttempt
    from ..models.webhook_endpoint_object_metadata import WebhookEndpointObjectMetadata


T = TypeVar("T", bound="WebhookEndpointObject")


@_attrs_define
class WebhookEndpointObject:
    """A webhook endpoint (design D11): where the account's events of one mode are delivered, signed
    with the account's webhook key of that mode.

        Example:
            {'created': 1790467200, 'description': 'Order fulfillment', 'disabled_reason': None, 'enabled_events':
                ['deposit.credited', 'deposit.reversed', 'refund.failed'], 'id': 'we_9e1c3a5b7d2f40c6e8a0b2d4f6a8c0e1',
                'last_attempt': {'at': 1790553695, 'status_code': 503}, 'livemode': False, 'metadata': {'team': 'payments'},
                'object': 'webhook_endpoint', 'oldest_pending_at': 1790553630, 'pending_deliveries': 2, 'status': 'enabled',
                'url': 'https://example.com/phala-pay/webhooks'}

        Attributes:
            created (int): Creation time, Unix seconds.
            enabled_events (list[str]): The event types delivered, or `["*"]` for all. Account events (`account.*`,
                `api_key.*`,
                `treasury.*`, `webhook_endpoint.*`) are delivered to every enabled endpoint whatever this
                lists.
            id (str): Endpoint id, `we_…`.
            livemode (bool): The endpoint's mode.
            metadata (WebhookEndpointObjectMetadata): Your key/value pairs
                ([metadata](https://docs.stripe.com/api/metadata)); `{}` when none.
            object_ (WebhookEndpointObjectObject): Always `webhook_endpoint`.
            pending_deliveries (int): Deliveries to the endpoint not delivered yet. They are retried with backoff (capped at
                an
                hour) until delivered and are never given up on; a count that keeps growing means the
                endpoint is failing: fix it, then list what it missed with
                `GET /v1/events?delivery_success=false`.
            status (str): `enabled` or `disabled`.
            url (str): Where events are delivered.
            deleted (bool | None | Unset): `true` in the `webhook_endpoint.deleted` event; absent otherwise.
            description (None | str | Unset): Your description.
            disabled_reason (None | str | Unset): `gone` when Phala Pay disabled the endpoint because it answered `410
                Gone`; `null`
                otherwise. Failing deliveries never disable an endpoint: they are retried until delivered.
            last_attempt (DeliveryAttempt | None | Unset):
            oldest_pending_at (int | None | Unset): Creation time of the oldest event not delivered to the endpoint yet,
                Unix seconds; `null`
                when none is pending.
    """

    created: int
    enabled_events: list[str]
    id: str
    livemode: bool
    metadata: WebhookEndpointObjectMetadata
    object_: WebhookEndpointObjectObject
    pending_deliveries: int
    status: str
    url: str
    deleted: bool | None | Unset = UNSET
    description: None | str | Unset = UNSET
    disabled_reason: None | str | Unset = UNSET
    last_attempt: DeliveryAttempt | None | Unset = UNSET
    oldest_pending_at: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.delivery_attempt import DeliveryAttempt  # noqa: PLC0415
        from ..models.webhook_endpoint_object_metadata import WebhookEndpointObjectMetadata  # noqa: PLC0415

        created = self.created

        enabled_events = self.enabled_events

        id = self.id

        livemode = self.livemode

        metadata = self.metadata.to_dict()

        object_: str = self.object_

        pending_deliveries = self.pending_deliveries

        status = self.status

        url = self.url

        deleted: bool | None | Unset
        if isinstance(self.deleted, Unset):
            deleted = UNSET
        else:
            deleted = self.deleted

        description: None | str | Unset
        if isinstance(self.description, Unset):
            description = UNSET
        else:
            description = self.description

        disabled_reason: None | str | Unset
        if isinstance(self.disabled_reason, Unset):
            disabled_reason = UNSET
        else:
            disabled_reason = self.disabled_reason

        last_attempt: dict[str, Any] | None | Unset
        if isinstance(self.last_attempt, Unset):
            last_attempt = UNSET
        elif isinstance(self.last_attempt, DeliveryAttempt):
            last_attempt = self.last_attempt.to_dict()
        else:
            last_attempt = self.last_attempt

        oldest_pending_at: int | None | Unset
        if isinstance(self.oldest_pending_at, Unset):
            oldest_pending_at = UNSET
        else:
            oldest_pending_at = self.oldest_pending_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "created": created,
                "enabled_events": enabled_events,
                "id": id,
                "livemode": livemode,
                "metadata": metadata,
                "object": object_,
                "pending_deliveries": pending_deliveries,
                "status": status,
                "url": url,
            }
        )
        if deleted is not UNSET:
            field_dict["deleted"] = deleted
        if description is not UNSET:
            field_dict["description"] = description
        if disabled_reason is not UNSET:
            field_dict["disabled_reason"] = disabled_reason
        if last_attempt is not UNSET:
            field_dict["last_attempt"] = last_attempt
        if oldest_pending_at is not UNSET:
            field_dict["oldest_pending_at"] = oldest_pending_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.delivery_attempt import DeliveryAttempt  # noqa: PLC0415
        from ..models.webhook_endpoint_object_metadata import WebhookEndpointObjectMetadata  # noqa: PLC0415

        d = dict(src_dict)
        created = d.pop("created")

        enabled_events = cast(list[str], d.pop("enabled_events"))

        id = d.pop("id")

        livemode = d.pop("livemode")

        metadata = WebhookEndpointObjectMetadata.from_dict(d.pop("metadata"))

        object_ = check_webhook_endpoint_object_object(d.pop("object"))

        pending_deliveries = d.pop("pending_deliveries")

        status = d.pop("status")

        url = d.pop("url")

        def _parse_deleted(data: object) -> bool | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(bool | None | Unset, data)

        deleted = _parse_deleted(d.pop("deleted", UNSET))

        def _parse_description(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        description = _parse_description(d.pop("description", UNSET))

        def _parse_disabled_reason(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        disabled_reason = _parse_disabled_reason(d.pop("disabled_reason", UNSET))

        def _parse_last_attempt(data: object) -> DeliveryAttempt | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                last_attempt_type_0 = DeliveryAttempt.from_dict(data)

                return last_attempt_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(DeliveryAttempt | None | Unset, data)

        last_attempt = _parse_last_attempt(d.pop("last_attempt", UNSET))

        def _parse_oldest_pending_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        oldest_pending_at = _parse_oldest_pending_at(d.pop("oldest_pending_at", UNSET))

        webhook_endpoint_object = cls(
            created=created,
            enabled_events=enabled_events,
            id=id,
            livemode=livemode,
            metadata=metadata,
            object_=object_,
            pending_deliveries=pending_deliveries,
            status=status,
            url=url,
            deleted=deleted,
            description=description,
            disabled_reason=disabled_reason,
            last_attempt=last_attempt,
            oldest_pending_at=oldest_pending_at,
        )

        webhook_endpoint_object.additional_properties = d
        return webhook_endpoint_object

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
