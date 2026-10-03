from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.deposit_list import DepositList
from ...models.error_response import ErrorResponse
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    client_reference_id: str | Unset = UNSET,
    quote: str | Unset = UNSET,
    deposit_address: str | Unset = UNSET,
    status: str | Unset = UNSET,
    tx_hash: str | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
    expand: list[str] | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    params["client_reference_id"] = client_reference_id

    params["quote"] = quote

    params["deposit_address"] = deposit_address

    params["status"] = status

    params["tx_hash"] = tx_hash

    params["created[gt]"] = createdgt

    params["created[gte]"] = createdgte

    params["created[lt]"] = createdlt

    params["created[lte]"] = createdlte

    params["limit"] = limit

    params["starting_after"] = starting_after

    params["ending_before"] = ending_before

    json_expand: list[str] | Unset = UNSET
    if not isinstance(expand, Unset):
        json_expand = expand

    params["expand[]"] = json_expand

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/deposits",
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> DepositList | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = DepositList.from_dict(response.json())

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
) -> Response[DepositList | ErrorResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    client_reference_id: str | Unset = UNSET,
    quote: str | Unset = UNSET,
    deposit_address: str | Unset = UNSET,
    status: str | Unset = UNSET,
    tx_hash: str | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
    expand: list[str] | Unset = UNSET,
) -> Response[DepositList | ErrorResponse]:
    """The account's deposits in the credential's mode, newest first, with Stripe's cursor
    pagination.

    Args:
        client_reference_id (str | Unset):
        quote (str | Unset):
        deposit_address (str | Unset):
        status (str | Unset):
        tx_hash (str | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):
        expand (list[str] | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[DepositList | ErrorResponse]
    """

    kwargs = _get_kwargs(
        client_reference_id=client_reference_id,
        quote=quote,
        deposit_address=deposit_address,
        status=status,
        tx_hash=tx_hash,
        createdgt=createdgt,
        createdgte=createdgte,
        createdlt=createdlt,
        createdlte=createdlte,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
        expand=expand,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    *,
    client: AuthenticatedClient,
    client_reference_id: str | Unset = UNSET,
    quote: str | Unset = UNSET,
    deposit_address: str | Unset = UNSET,
    status: str | Unset = UNSET,
    tx_hash: str | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
    expand: list[str] | Unset = UNSET,
) -> DepositList | ErrorResponse | None:
    """The account's deposits in the credential's mode, newest first, with Stripe's cursor
    pagination.

    Args:
        client_reference_id (str | Unset):
        quote (str | Unset):
        deposit_address (str | Unset):
        status (str | Unset):
        tx_hash (str | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):
        expand (list[str] | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        DepositList | ErrorResponse
    """

    return sync_detailed(
        client=client,
        client_reference_id=client_reference_id,
        quote=quote,
        deposit_address=deposit_address,
        status=status,
        tx_hash=tx_hash,
        createdgt=createdgt,
        createdgte=createdgte,
        createdlt=createdlt,
        createdlte=createdlte,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
        expand=expand,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    client_reference_id: str | Unset = UNSET,
    quote: str | Unset = UNSET,
    deposit_address: str | Unset = UNSET,
    status: str | Unset = UNSET,
    tx_hash: str | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
    expand: list[str] | Unset = UNSET,
) -> Response[DepositList | ErrorResponse]:
    """The account's deposits in the credential's mode, newest first, with Stripe's cursor
    pagination.

    Args:
        client_reference_id (str | Unset):
        quote (str | Unset):
        deposit_address (str | Unset):
        status (str | Unset):
        tx_hash (str | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):
        expand (list[str] | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[DepositList | ErrorResponse]
    """

    kwargs = _get_kwargs(
        client_reference_id=client_reference_id,
        quote=quote,
        deposit_address=deposit_address,
        status=status,
        tx_hash=tx_hash,
        createdgt=createdgt,
        createdgte=createdgte,
        createdlt=createdlt,
        createdlte=createdlte,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
        expand=expand,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    *,
    client: AuthenticatedClient,
    client_reference_id: str | Unset = UNSET,
    quote: str | Unset = UNSET,
    deposit_address: str | Unset = UNSET,
    status: str | Unset = UNSET,
    tx_hash: str | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
    expand: list[str] | Unset = UNSET,
) -> DepositList | ErrorResponse | None:
    """The account's deposits in the credential's mode, newest first, with Stripe's cursor
    pagination.

    Args:
        client_reference_id (str | Unset):
        quote (str | Unset):
        deposit_address (str | Unset):
        status (str | Unset):
        tx_hash (str | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):
        expand (list[str] | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        DepositList | ErrorResponse
    """

    return (
        await asyncio_detailed(
            client=client,
            client_reference_id=client_reference_id,
            quote=quote,
            deposit_address=deposit_address,
            status=status,
            tx_hash=tx_hash,
            createdgt=createdgt,
            createdgte=createdgte,
            createdlt=createdlt,
            createdlte=createdlte,
            limit=limit,
            starting_after=starting_after,
            ending_before=ending_before,
            expand=expand,
        )
    ).parsed
