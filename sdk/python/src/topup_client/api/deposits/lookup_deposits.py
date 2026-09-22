from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.support_deposits_response import SupportDepositsResponse
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    p: str,
    *,
    tx_hash: str | Unset = UNSET,
    address: str | Unset = UNSET,
    lock_ref: str | Unset = UNSET,
    cursor: str | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    params["tx_hash"] = tx_hash

    params["address"] = address

    params["lock_ref"] = lock_ref

    params["cursor"] = cursor

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/products/{p}/deposits".format(
            p=quote(str(p), safe=""),
        ),
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | SupportDepositsResponse | None:
    if response.status_code == 200:
        response_200 = SupportDepositsResponse.from_dict(response.json())

        return response_200

    if response.status_code == 400:
        response_400 = ErrorResponse.from_dict(response.json())

        return response_400

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ErrorResponse | SupportDepositsResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    p: str,
    *,
    client: AuthenticatedClient,
    tx_hash: str | Unset = UNSET,
    address: str | Unset = UNSET,
    lock_ref: str | Unset = UNSET,
    cursor: str | Unset = UNSET,
) -> Response[ErrorResponse | SupportDepositsResponse]:
    """
    Args:
        p (str):
        tx_hash (str | Unset):
        address (str | Unset):
        lock_ref (str | Unset):
        cursor (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | SupportDepositsResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        tx_hash=tx_hash,
        address=address,
        lock_ref=lock_ref,
        cursor=cursor,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    p: str,
    *,
    client: AuthenticatedClient,
    tx_hash: str | Unset = UNSET,
    address: str | Unset = UNSET,
    lock_ref: str | Unset = UNSET,
    cursor: str | Unset = UNSET,
) -> ErrorResponse | SupportDepositsResponse | None:
    """
    Args:
        p (str):
        tx_hash (str | Unset):
        address (str | Unset):
        lock_ref (str | Unset):
        cursor (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | SupportDepositsResponse
    """

    return sync_detailed(
        p=p,
        client=client,
        tx_hash=tx_hash,
        address=address,
        lock_ref=lock_ref,
        cursor=cursor,
    ).parsed


async def asyncio_detailed(
    p: str,
    *,
    client: AuthenticatedClient,
    tx_hash: str | Unset = UNSET,
    address: str | Unset = UNSET,
    lock_ref: str | Unset = UNSET,
    cursor: str | Unset = UNSET,
) -> Response[ErrorResponse | SupportDepositsResponse]:
    """
    Args:
        p (str):
        tx_hash (str | Unset):
        address (str | Unset):
        lock_ref (str | Unset):
        cursor (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | SupportDepositsResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        tx_hash=tx_hash,
        address=address,
        lock_ref=lock_ref,
        cursor=cursor,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    p: str,
    *,
    client: AuthenticatedClient,
    tx_hash: str | Unset = UNSET,
    address: str | Unset = UNSET,
    lock_ref: str | Unset = UNSET,
    cursor: str | Unset = UNSET,
) -> ErrorResponse | SupportDepositsResponse | None:
    """
    Args:
        p (str):
        tx_hash (str | Unset):
        address (str | Unset):
        lock_ref (str | Unset):
        cursor (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | SupportDepositsResponse
    """

    return (
        await asyncio_detailed(
            p=p,
            client=client,
            tx_hash=tx_hash,
            address=address,
            lock_ref=lock_ref,
            cursor=cursor,
        )
    ).parsed
