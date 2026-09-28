from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.api_key_object import ApiKeyObject
from ...models.error_response import ErrorResponse
from ...models.roll_api_key_request import RollApiKeyRequest
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    id: str,
    *,
    body: RollApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/api_keys/{id}/roll".format(
            id=quote(str(id), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ApiKeyObject | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = ApiKeyObject.from_dict(response.json())

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
) -> Response[ApiKeyObject | ErrorResponse]:
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
    body: RollApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ApiKeyObject | ErrorResponse]:
    """Rolls a key: returns a new secret key with the same name, and the old key keeps working for
    `expires_in` seconds (at most 7 days), Stripe's roll; `0`, the default, revokes it at once. A
    key may roll itself.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (RollApiKeyRequest): `POST /v1/api_keys/{id}/roll` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ApiKeyObject | ErrorResponse]
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
    body: RollApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ApiKeyObject | ErrorResponse | None:
    """Rolls a key: returns a new secret key with the same name, and the old key keeps working for
    `expires_in` seconds (at most 7 days), Stripe's roll; `0`, the default, revokes it at once. A
    key may roll itself.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (RollApiKeyRequest): `POST /v1/api_keys/{id}/roll` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ApiKeyObject | ErrorResponse
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
    body: RollApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ApiKeyObject | ErrorResponse]:
    """Rolls a key: returns a new secret key with the same name, and the old key keeps working for
    `expires_in` seconds (at most 7 days), Stripe's roll; `0`, the default, revokes it at once. A
    key may roll itself.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (RollApiKeyRequest): `POST /v1/api_keys/{id}/roll` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ApiKeyObject | ErrorResponse]
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
    body: RollApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ApiKeyObject | ErrorResponse | None:
    """Rolls a key: returns a new secret key with the same name, and the old key keeps working for
    `expires_in` seconds (at most 7 days), Stripe's roll; `0`, the default, revokes it at once. A
    key may roll itself.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (RollApiKeyRequest): `POST /v1/api_keys/{id}/roll` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ApiKeyObject | ErrorResponse
    """

    return (
        await asyncio_detailed(
            id=id,
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
