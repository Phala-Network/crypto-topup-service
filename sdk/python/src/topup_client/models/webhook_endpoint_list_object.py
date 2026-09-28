from typing import Literal

WebhookEndpointListObject = Literal["list"]

WEBHOOK_ENDPOINT_LIST_OBJECT_VALUES: set[WebhookEndpointListObject] = {
    "list",
}


def check_webhook_endpoint_list_object(value: str) -> WebhookEndpointListObject:
    if value in WEBHOOK_ENDPOINT_LIST_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {WEBHOOK_ENDPOINT_LIST_OBJECT_VALUES!r}"
    )
