from typing import Literal

DeletedWebhookEndpointObject = Literal["webhook_endpoint"]

DELETED_WEBHOOK_ENDPOINT_OBJECT_VALUES: set[DeletedWebhookEndpointObject] = {
    "webhook_endpoint",
}


def check_deleted_webhook_endpoint_object(value: str) -> DeletedWebhookEndpointObject:
    if value in DELETED_WEBHOOK_ENDPOINT_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {DELETED_WEBHOOK_ENDPOINT_OBJECT_VALUES!r}"
    )
