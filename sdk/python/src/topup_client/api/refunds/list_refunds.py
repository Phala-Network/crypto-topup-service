from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.refund_list import RefundList
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    deposit: str | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    params["deposit"] = deposit

    params["status"] = status

    params["limit"] = limit

    params["starting_after"] = starting_after

    params["ending_before"] = ending_before

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/refunds",
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | RefundList | None:
    if response.status_code == 200:
        response_200 = RefundList.from_dict(response.json())

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
) -> Response[ErrorResponse | RefundList]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    deposit: str | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> Response[ErrorResponse | RefundList]:
    """The account's refunds in the key's mode, newest first, with Stripe's cursor pagination.

    Args:
        deposit (str | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | RefundList]
    """

    kwargs = _get_kwargs(
        deposit=deposit,
        status=status,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    *,
    client: AuthenticatedClient,
    deposit: str | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> ErrorResponse | RefundList | None:
    """The account's refunds in the key's mode, newest first, with Stripe's cursor pagination.

    Args:
        deposit (str | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | RefundList
    """

    return sync_detailed(
        client=client,
        deposit=deposit,
        status=status,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    deposit: str | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> Response[ErrorResponse | RefundList]:
    """The account's refunds in the key's mode, newest first, with Stripe's cursor pagination.

    Args:
        deposit (str | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | RefundList]
    """

    kwargs = _get_kwargs(
        deposit=deposit,
        status=status,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    *,
    client: AuthenticatedClient,
    deposit: str | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> ErrorResponse | RefundList | None:
    """The account's refunds in the key's mode, newest first, with Stripe's cursor pagination.

    Args:
        deposit (str | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | RefundList
    """

    return (
        await asyncio_detailed(
            client=client,
            deposit=deposit,
            status=status,
            limit=limit,
            starting_after=starting_after,
            ending_before=ending_before,
        )
    ).parsed
