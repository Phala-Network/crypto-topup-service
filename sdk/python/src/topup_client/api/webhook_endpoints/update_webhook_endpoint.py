from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.update_webhook_endpoint_request import UpdateWebhookEndpointRequest
from ...models.webhook_endpoint_object import WebhookEndpointObject
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    id: str,
    *,
    body: UpdateWebhookEndpointRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/webhook_endpoints/{id}".format(
            id=quote(str(id), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | WebhookEndpointObject | None:
    if response.status_code == 200:
        response_200 = WebhookEndpointObject.from_dict(response.json())

        return response_200

    if response.status_code == 400:
        response_400 = ErrorResponse.from_dict(response.json())

        return response_400

    if response.status_code == 401:
        response_401 = ErrorResponse.from_dict(response.json())

        return response_401

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

    if response.status_code == 409:
        response_409 = ErrorResponse.from_dict(response.json())

        return response_409

    if response.status_code == 429:
        response_429 = ErrorResponse.from_dict(response.json())

        return response_429

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ErrorResponse | WebhookEndpointObject]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateWebhookEndpointRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | WebhookEndpointObject]:
    """Updates a webhook endpoint; parameters not sent are left unchanged. A change is announced as
    `webhook_endpoint.updated`, with the replaced values in `data.previous_attributes`, to every
    enabled endpoint, and first to this endpoint at the URL it had before, even when the change
    disables it. Disabling stops its pending deliveries; enabling does not restart them (resend
    with `POST /v1/events/{id}/resend`).

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateWebhookEndpointRequest): `POST /v1/webhook_endpoints/{id}` body; parameters
            not sent are left unchanged. Example: {'disabled': False, 'enabled_events': ['*']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | WebhookEndpointObject]
    """

    kwargs = _get_kwargs(
        id=id,
        body=body,
        idempotency_key=idempotency_key,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateWebhookEndpointRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | WebhookEndpointObject | None:
    """Updates a webhook endpoint; parameters not sent are left unchanged. A change is announced as
    `webhook_endpoint.updated`, with the replaced values in `data.previous_attributes`, to every
    enabled endpoint, and first to this endpoint at the URL it had before, even when the change
    disables it. Disabling stops its pending deliveries; enabling does not restart them (resend
    with `POST /v1/events/{id}/resend`).

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateWebhookEndpointRequest): `POST /v1/webhook_endpoints/{id}` body; parameters
            not sent are left unchanged. Example: {'disabled': False, 'enabled_events': ['*']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | WebhookEndpointObject
    """

    return sync_detailed(
        id=id,
        client=client,
        body=body,
        idempotency_key=idempotency_key,
    ).parsed


async def asyncio_detailed(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateWebhookEndpointRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | WebhookEndpointObject]:
    """Updates a webhook endpoint; parameters not sent are left unchanged. A change is announced as
    `webhook_endpoint.updated`, with the replaced values in `data.previous_attributes`, to every
    enabled endpoint, and first to this endpoint at the URL it had before, even when the change
    disables it. Disabling stops its pending deliveries; enabling does not restart them (resend
    with `POST /v1/events/{id}/resend`).

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateWebhookEndpointRequest): `POST /v1/webhook_endpoints/{id}` body; parameters
            not sent are left unchanged. Example: {'disabled': False, 'enabled_events': ['*']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | WebhookEndpointObject]
    """

    kwargs = _get_kwargs(
        id=id,
        body=body,
        idempotency_key=idempotency_key,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateWebhookEndpointRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | WebhookEndpointObject | None:
    """Updates a webhook endpoint; parameters not sent are left unchanged. A change is announced as
    `webhook_endpoint.updated`, with the replaced values in `data.previous_attributes`, to every
    enabled endpoint, and first to this endpoint at the URL it had before, even when the change
    disables it. Disabling stops its pending deliveries; enabling does not restart them (resend
    with `POST /v1/events/{id}/resend`).

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateWebhookEndpointRequest): `POST /v1/webhook_endpoints/{id}` body; parameters
            not sent are left unchanged. Example: {'disabled': False, 'enabled_events': ['*']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | WebhookEndpointObject
    """

    return (
        await asyncio_detailed(
            id=id,
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
