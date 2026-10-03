from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.api_key_object import ApiKeyObject
from ...models.create_api_key_request import CreateApiKeyRequest
from ...models.error_response import ErrorResponse
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    body: CreateApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/api_keys",
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

    if response.status_code == 403:
        response_403 = ErrorResponse.from_dict(response.json())

        return response_403

    if response.status_code == 409:
        response_409 = ErrorResponse.from_dict(response.json())

        return response_409

    if response.status_code == 429:
        response_429 = ErrorResponse.from_dict(response.json())

        return response_429

    if response.status_code == 503:
        response_503 = ErrorResponse.from_dict(response.json())

        return response_503

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
    *,
    client: AuthenticatedClient,
    body: CreateApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ApiKeyObject | ErrorResponse]:
    """Creates a key in the requesting key's mode: a secret key, or with `type: restricted` a
    restricted key (`ppay_rk_…`) holding only `permissions`, Stripe's restricted keys. Run
    production servers with a restricted key and keep secret keys for administration. The
    response is the only time its `secret` is shown; a replay of the request (`Idempotency-Key`)
    returns the key without it.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateApiKeyRequest): `POST /v1/api_keys` body. Example: {'name': 'fulfillment
            worker', 'permissions': ['quotes.write', 'deposit_addresses.write', 'deposits.read',
            'events.read', 'refunds.read'], 'type': 'restricted'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ApiKeyObject | ErrorResponse]
    """

    kwargs = _get_kwargs(
        body=body,
        idempotency_key=idempotency_key,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    *,
    client: AuthenticatedClient,
    body: CreateApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ApiKeyObject | ErrorResponse | None:
    """Creates a key in the requesting key's mode: a secret key, or with `type: restricted` a
    restricted key (`ppay_rk_…`) holding only `permissions`, Stripe's restricted keys. Run
    production servers with a restricted key and keep secret keys for administration. The
    response is the only time its `secret` is shown; a replay of the request (`Idempotency-Key`)
    returns the key without it.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateApiKeyRequest): `POST /v1/api_keys` body. Example: {'name': 'fulfillment
            worker', 'permissions': ['quotes.write', 'deposit_addresses.write', 'deposits.read',
            'events.read', 'refunds.read'], 'type': 'restricted'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ApiKeyObject | ErrorResponse
    """

    return sync_detailed(
        client=client,
        body=body,
        idempotency_key=idempotency_key,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    body: CreateApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ApiKeyObject | ErrorResponse]:
    """Creates a key in the requesting key's mode: a secret key, or with `type: restricted` a
    restricted key (`ppay_rk_…`) holding only `permissions`, Stripe's restricted keys. Run
    production servers with a restricted key and keep secret keys for administration. The
    response is the only time its `secret` is shown; a replay of the request (`Idempotency-Key`)
    returns the key without it.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateApiKeyRequest): `POST /v1/api_keys` body. Example: {'name': 'fulfillment
            worker', 'permissions': ['quotes.write', 'deposit_addresses.write', 'deposits.read',
            'events.read', 'refunds.read'], 'type': 'restricted'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ApiKeyObject | ErrorResponse]
    """

    kwargs = _get_kwargs(
        body=body,
        idempotency_key=idempotency_key,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    *,
    client: AuthenticatedClient,
    body: CreateApiKeyRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ApiKeyObject | ErrorResponse | None:
    """Creates a key in the requesting key's mode: a secret key, or with `type: restricted` a
    restricted key (`ppay_rk_…`) holding only `permissions`, Stripe's restricted keys. Run
    production servers with a restricted key and keep secret keys for administration. The
    response is the only time its `secret` is shown; a replay of the request (`Idempotency-Key`)
    returns the key without it.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateApiKeyRequest): `POST /v1/api_keys` body. Example: {'name': 'fulfillment
            worker', 'permissions': ['quotes.write', 'deposit_addresses.write', 'deposits.read',
            'events.read', 'refunds.read'], 'type': 'restricted'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ApiKeyObject | ErrorResponse
    """

    return (
        await asyncio_detailed(
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
