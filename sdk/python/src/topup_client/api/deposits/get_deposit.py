from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.deposit_response import DepositResponse
from ...models.error_response import ErrorResponse
from typing import cast
from uuid import UUID


def _get_kwargs(
    p: str,
    id: UUID,
) -> dict[str, Any]:

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/products/{p}/deposits/{id}".format(
            p=quote(str(p), safe=""),
            id=quote(str(id), safe=""),
        ),
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> DepositResponse | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = DepositResponse.from_dict(response.json())

        return response_200

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[DepositResponse | ErrorResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    p: str,
    id: UUID,
    *,
    client: AuthenticatedClient,
) -> Response[DepositResponse | ErrorResponse]:
    """
    Args:
        p (str):
        id (UUID):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[DepositResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        id=id,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    p: str,
    id: UUID,
    *,
    client: AuthenticatedClient,
) -> DepositResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        id (UUID):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        DepositResponse | ErrorResponse
    """

    return sync_detailed(
        p=p,
        id=id,
        client=client,
    ).parsed


async def asyncio_detailed(
    p: str,
    id: UUID,
    *,
    client: AuthenticatedClient,
) -> Response[DepositResponse | ErrorResponse]:
    """
    Args:
        p (str):
        id (UUID):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[DepositResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        id=id,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    p: str,
    id: UUID,
    *,
    client: AuthenticatedClient,
) -> DepositResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        id (UUID):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        DepositResponse | ErrorResponse
    """

    return (
        await asyncio_detailed(
            p=p,
            id=id,
            client=client,
        )
    ).parsed
