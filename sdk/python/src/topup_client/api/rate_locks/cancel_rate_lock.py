from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.cancel_rate_lock_response import CancelRateLockResponse
from ...models.error_response import ErrorResponse
from typing import cast


def _get_kwargs(
    p: str,
    ext: str,
    ref: str,
) -> dict[str, Any]:

    _kwargs: dict[str, Any] = {
        "method": "delete",
        "url": "/v1/products/{p}/accounts/{ext}/rate-locks/{ref}".format(
            p=quote(str(p), safe=""),
            ext=quote(str(ext), safe=""),
            ref=quote(str(ref), safe=""),
        ),
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> CancelRateLockResponse | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = CancelRateLockResponse.from_dict(response.json())

        return response_200

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
) -> Response[CancelRateLockResponse | ErrorResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    p: str,
    ext: str,
    ref: str,
    *,
    client: AuthenticatedClient,
) -> Response[CancelRateLockResponse | ErrorResponse]:
    """
    Args:
        p (str):
        ext (str):
        ref (str):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[CancelRateLockResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        ext=ext,
        ref=ref,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    p: str,
    ext: str,
    ref: str,
    *,
    client: AuthenticatedClient,
) -> CancelRateLockResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        ext (str):
        ref (str):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        CancelRateLockResponse | ErrorResponse
    """

    return sync_detailed(
        p=p,
        ext=ext,
        ref=ref,
        client=client,
    ).parsed


async def asyncio_detailed(
    p: str,
    ext: str,
    ref: str,
    *,
    client: AuthenticatedClient,
) -> Response[CancelRateLockResponse | ErrorResponse]:
    """
    Args:
        p (str):
        ext (str):
        ref (str):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[CancelRateLockResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        ext=ext,
        ref=ref,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    p: str,
    ext: str,
    ref: str,
    *,
    client: AuthenticatedClient,
) -> CancelRateLockResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        ext (str):
        ref (str):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        CancelRateLockResponse | ErrorResponse
    """

    return (
        await asyncio_detailed(
            p=p,
            ext=ext,
            ref=ref,
            client=client,
        )
    ).parsed
