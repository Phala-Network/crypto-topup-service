from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.pause_request import PauseRequest
from ...models.pause_response import PauseResponse
from typing import cast


def _get_kwargs(
    account: str,
    customer: str,
    *,
    body: PauseRequest,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/admin/accounts/{account}/customers/{customer}/pause".format(
            account=quote(str(account), safe=""),
            customer=quote(str(customer), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | PauseResponse | None:
    if response.status_code == 200:
        response_200 = PauseResponse.from_dict(response.json())

        return response_200

    if response.status_code == 400:
        response_400 = ErrorResponse.from_dict(response.json())

        return response_400

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ErrorResponse | PauseResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    account: str,
    customer: str,
    *,
    client: AuthenticatedClient,
    body: PauseRequest,
) -> Response[ErrorResponse | PauseResponse]:
    """Pauses scopes of one customer of an account, for example `settlement` to stop crediting it.

    Args:
        account (str):
        customer (str):
        body (PauseRequest): Pause or resume request.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | PauseResponse]
    """

    kwargs = _get_kwargs(
        account=account,
        customer=customer,
        body=body,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    account: str,
    customer: str,
    *,
    client: AuthenticatedClient,
    body: PauseRequest,
) -> ErrorResponse | PauseResponse | None:
    """Pauses scopes of one customer of an account, for example `settlement` to stop crediting it.

    Args:
        account (str):
        customer (str):
        body (PauseRequest): Pause or resume request.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | PauseResponse
    """

    return sync_detailed(
        account=account,
        customer=customer,
        client=client,
        body=body,
    ).parsed


async def asyncio_detailed(
    account: str,
    customer: str,
    *,
    client: AuthenticatedClient,
    body: PauseRequest,
) -> Response[ErrorResponse | PauseResponse]:
    """Pauses scopes of one customer of an account, for example `settlement` to stop crediting it.

    Args:
        account (str):
        customer (str):
        body (PauseRequest): Pause or resume request.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | PauseResponse]
    """

    kwargs = _get_kwargs(
        account=account,
        customer=customer,
        body=body,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    account: str,
    customer: str,
    *,
    client: AuthenticatedClient,
    body: PauseRequest,
) -> ErrorResponse | PauseResponse | None:
    """Pauses scopes of one customer of an account, for example `settlement` to stop crediting it.

    Args:
        account (str):
        customer (str):
        body (PauseRequest): Pause or resume request.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | PauseResponse
    """

    return (
        await asyncio_detailed(
            account=account,
            customer=customer,
            client=client,
            body=body,
        )
    ).parsed
