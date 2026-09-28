from typing import Literal

WebhookEndpointObjectObject = Literal["webhook_endpoint"]

WEBHOOK_ENDPOINT_OBJECT_OBJECT_VALUES: set[WebhookEndpointObjectObject] = {
    "webhook_endpoint",
}


def check_webhook_endpoint_object_object(value: str) -> WebhookEndpointObjectObject:
    if value in WEBHOOK_ENDPOINT_OBJECT_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {WEBHOOK_ENDPOINT_OBJECT_OBJECT_VALUES!r}"
    )
