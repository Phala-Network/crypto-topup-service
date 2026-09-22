from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.refund_request import RefundRequest
from ...models.refund_response import RefundResponse
from typing import cast
from uuid import UUID


def _get_kwargs(
    p: str,
    id: UUID,
    *,
    body: RefundRequest,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/products/{p}/deposits/{id}/refund-requests".format(
            p=quote(str(p), safe=""),
            id=quote(str(id), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | RefundResponse | None:
    if response.status_code == 200:
        response_200 = RefundResponse.from_dict(response.json())

        return response_200

    if response.status_code == 400:
        response_400 = ErrorResponse.from_dict(response.json())

        return response_400

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

    if response.status_code == 409:
        response_409 = ErrorResponse.from_dict(response.json())

        return response_409

    if response.status_code == 423:
        response_423 = ErrorResponse.from_dict(response.json())

        return response_423

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ErrorResponse | RefundResponse]:
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
    body: RefundRequest,
) -> Response[ErrorResponse | RefundResponse]:
    """
    Args:
        p (str):
        id (UUID):
        body (RefundRequest): Refund request body owned by C12.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | RefundResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        id=id,
        body=body,
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
    body: RefundRequest,
) -> ErrorResponse | RefundResponse | None:
    """
    Args:
        p (str):
        id (UUID):
        body (RefundRequest): Refund request body owned by C12.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | RefundResponse
    """

    return sync_detailed(
        p=p,
        id=id,
        client=client,
        body=body,
    ).parsed


async def asyncio_detailed(
    p: str,
    id: UUID,
    *,
    client: AuthenticatedClient,
    body: RefundRequest,
) -> Response[ErrorResponse | RefundResponse]:
    """
    Args:
        p (str):
        id (UUID):
        body (RefundRequest): Refund request body owned by C12.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | RefundResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        id=id,
        body=body,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    p: str,
    id: UUID,
    *,
    client: AuthenticatedClient,
    body: RefundRequest,
) -> ErrorResponse | RefundResponse | None:
    """
    Args:
        p (str):
        id (UUID):
        body (RefundRequest): Refund request body owned by C12.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | RefundResponse
    """

    return (
        await asyncio_detailed(
            p=p,
            id=id,
            client=client,
            body=body,
        )
    ).parsed
