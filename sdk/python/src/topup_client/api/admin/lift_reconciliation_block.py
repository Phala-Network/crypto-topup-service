from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.admin_reason_request import AdminReasonRequest
from ...models.error_response import ErrorResponse
from ...models.reconciliation_block_lift_response import ReconciliationBlockLiftResponse
from typing import cast


def _get_kwargs(
    block_key: str,
    *,
    body: AdminReasonRequest,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/admin/reconciliation-blocks/{block_key}/lift".format(
            block_key=quote(str(block_key), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | ReconciliationBlockLiftResponse | None:
    if response.status_code == 200:
        response_200 = ReconciliationBlockLiftResponse.from_dict(response.json())

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
) -> Response[ErrorResponse | ReconciliationBlockLiftResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    block_key: str,
    *,
    client: AuthenticatedClient,
    body: AdminReasonRequest,
) -> Response[ErrorResponse | ReconciliationBlockLiftResponse]:
    """
    Args:
        block_key (str):
        body (AdminReasonRequest): Administrative action body; `reason` is recorded in the
            action's audit row.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | ReconciliationBlockLiftResponse]
    """

    kwargs = _get_kwargs(
        block_key=block_key,
        body=body,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    block_key: str,
    *,
    client: AuthenticatedClient,
    body: AdminReasonRequest,
) -> ErrorResponse | ReconciliationBlockLiftResponse | None:
    """
    Args:
        block_key (str):
        body (AdminReasonRequest): Administrative action body; `reason` is recorded in the
            action's audit row.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | ReconciliationBlockLiftResponse
    """

    return sync_detailed(
        block_key=block_key,
        client=client,
        body=body,
    ).parsed


async def asyncio_detailed(
    block_key: str,
    *,
    client: AuthenticatedClient,
    body: AdminReasonRequest,
) -> Response[ErrorResponse | ReconciliationBlockLiftResponse]:
    """
    Args:
        block_key (str):
        body (AdminReasonRequest): Administrative action body; `reason` is recorded in the
            action's audit row.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | ReconciliationBlockLiftResponse]
    """

    kwargs = _get_kwargs(
        block_key=block_key,
        body=body,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    block_key: str,
    *,
    client: AuthenticatedClient,
    body: AdminReasonRequest,
) -> ErrorResponse | ReconciliationBlockLiftResponse | None:
    """
    Args:
        block_key (str):
        body (AdminReasonRequest): Administrative action body; `reason` is recorded in the
            action's audit row.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | ReconciliationBlockLiftResponse
    """

    return (
        await asyncio_detailed(
            block_key=block_key,
            client=client,
            body=body,
        )
    ).parsed
