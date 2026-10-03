from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.treasury_list import TreasuryList
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    chain_id: int | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    params["chain_id"] = chain_id

    params["status"] = status

    params["limit"] = limit

    params["starting_after"] = starting_after

    params["ending_before"] = ending_before

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/treasuries",
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | TreasuryList | None:
    if response.status_code == 200:
        response_200 = TreasuryList.from_dict(response.json())

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
) -> Response[ErrorResponse | TreasuryList]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    chain_id: int | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> Response[ErrorResponse | TreasuryList]:
    """The account's treasuries in the key's mode, newest first, with Stripe's cursor pagination: each
    chain's `active` one, any `pending` change, and the `replaced` and `canceled` ones.

    Args:
        chain_id (int | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | TreasuryList]
    """

    kwargs = _get_kwargs(
        chain_id=chain_id,
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
    chain_id: int | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> ErrorResponse | TreasuryList | None:
    """The account's treasuries in the key's mode, newest first, with Stripe's cursor pagination: each
    chain's `active` one, any `pending` change, and the `replaced` and `canceled` ones.

    Args:
        chain_id (int | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | TreasuryList
    """

    return sync_detailed(
        client=client,
        chain_id=chain_id,
        status=status,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    chain_id: int | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> Response[ErrorResponse | TreasuryList]:
    """The account's treasuries in the key's mode, newest first, with Stripe's cursor pagination: each
    chain's `active` one, any `pending` change, and the `replaced` and `canceled` ones.

    Args:
        chain_id (int | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | TreasuryList]
    """

    kwargs = _get_kwargs(
        chain_id=chain_id,
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
    chain_id: int | Unset = UNSET,
    status: str | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> ErrorResponse | TreasuryList | None:
    """The account's treasuries in the key's mode, newest first, with Stripe's cursor pagination: each
    chain's `active` one, any `pending` change, and the `replaced` and `canceled` ones.

    Args:
        chain_id (int | Unset):
        status (str | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | TreasuryList
    """

    return (
        await asyncio_detailed(
            client=client,
            chain_id=chain_id,
            status=status,
            limit=limit,
            starting_after=starting_after,
            ending_before=ending_before,
        )
    ).parsed
