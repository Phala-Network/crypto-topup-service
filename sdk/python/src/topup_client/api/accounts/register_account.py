from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.account_response import AccountResponse
from ...models.error_response import ErrorResponse
from ...models.register_account_request import RegisterAccountRequest
from typing import cast


def _get_kwargs(
    p: str,
    *,
    body: RegisterAccountRequest,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/products/{p}/accounts".format(
            p=quote(str(p), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> AccountResponse | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = AccountResponse.from_dict(response.json())

        return response_200

    if response.status_code == 400:
        response_400 = ErrorResponse.from_dict(response.json())

        return response_400

    if response.status_code == 401:
        response_401 = ErrorResponse.from_dict(response.json())

        return response_401

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[AccountResponse | ErrorResponse]:
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
    body: RegisterAccountRequest,
) -> Response[AccountResponse | ErrorResponse]:
    """
    Args:
        p (str):
        body (RegisterAccountRequest): Account registration body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[AccountResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        body=body,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    p: str,
    *,
    client: AuthenticatedClient,
    body: RegisterAccountRequest,
) -> AccountResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        body (RegisterAccountRequest): Account registration body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        AccountResponse | ErrorResponse
    """

    return sync_detailed(
        p=p,
        client=client,
        body=body,
    ).parsed


async def asyncio_detailed(
    p: str,
    *,
    client: AuthenticatedClient,
    body: RegisterAccountRequest,
) -> Response[AccountResponse | ErrorResponse]:
    """
    Args:
        p (str):
        body (RegisterAccountRequest): Account registration body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[AccountResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        body=body,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    p: str,
    *,
    client: AuthenticatedClient,
    body: RegisterAccountRequest,
) -> AccountResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        body (RegisterAccountRequest): Account registration body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        AccountResponse | ErrorResponse
    """

    return (
        await asyncio_detailed(
            p=p,
            client=client,
            body=body,
        )
    ).parsed
