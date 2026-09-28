from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.event_object_response import EventObjectResponse
from ...models.resend_event_request import ResendEventRequest
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    id: str,
    *,
    body: ResendEventRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/events/{id}/resend".format(
            id=quote(str(id), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | EventObjectResponse | None:
    if response.status_code == 200:
        response_200 = EventObjectResponse.from_dict(response.json())

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

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ErrorResponse | EventObjectResponse]:
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
    body: ResendEventRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | EventObjectResponse]:
    """Delivers an event again to one enabled endpoint, whether it was delivered there, stopped, or
    never sent there (the Stripe CLI's `events resend`), with the same `webhook-id` and body. Use
    it after re-enabling an endpoint for the events it missed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (ResendEventRequest): `POST /v1/events/{id}/resend` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | EventObjectResponse]
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
    body: ResendEventRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | EventObjectResponse | None:
    """Delivers an event again to one enabled endpoint, whether it was delivered there, stopped, or
    never sent there (the Stripe CLI's `events resend`), with the same `webhook-id` and body. Use
    it after re-enabling an endpoint for the events it missed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (ResendEventRequest): `POST /v1/events/{id}/resend` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | EventObjectResponse
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
    body: ResendEventRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | EventObjectResponse]:
    """Delivers an event again to one enabled endpoint, whether it was delivered there, stopped, or
    never sent there (the Stripe CLI's `events resend`), with the same `webhook-id` and body. Use
    it after re-enabling an endpoint for the events it missed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (ResendEventRequest): `POST /v1/events/{id}/resend` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | EventObjectResponse]
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
    body: ResendEventRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | EventObjectResponse | None:
    """Delivers an event again to one enabled endpoint, whether it was delivered there, stopped, or
    never sent there (the Stripe CLI's `events resend`), with the same `webhook-id` and body. Use
    it after re-enabling an endpoint for the events it missed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (ResendEventRequest): `POST /v1/events/{id}/resend` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | EventObjectResponse
    """

    return (
        await asyncio_detailed(
            id=id,
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
