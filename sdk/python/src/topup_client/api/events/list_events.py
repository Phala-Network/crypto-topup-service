from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.error_response import ErrorResponse
from ...models.event_list import EventList
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    type_: str | Unset = UNSET,
    types: list[str] | Unset = UNSET,
    delivery_success: bool | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    params["type"] = type_

    json_types: list[str] | Unset = UNSET
    if not isinstance(types, Unset):
        json_types = types

    params["types[]"] = json_types

    params["delivery_success"] = delivery_success

    params["created[gt]"] = createdgt

    params["created[gte]"] = createdgte

    params["created[lt]"] = createdlt

    params["created[lte]"] = createdlte

    params["limit"] = limit

    params["starting_after"] = starting_after

    params["ending_before"] = ending_before

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/events",
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | EventList | None:
    if response.status_code == 200:
        response_200 = EventList.from_dict(response.json())

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
) -> Response[ErrorResponse | EventList]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    type_: str | Unset = UNSET,
    types: list[str] | Unset = UNSET,
    delivery_success: bool | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> Response[ErrorResponse | EventList]:
    """The account's events in the key's mode, newest first, with Stripe's cursor pagination: the
    notifications webhooks deliver, and the audit log of every key, endpoint, and account change
    with its `actor`. An event stays listed whether or not any endpoint received it.

    Args:
        type_ (str | Unset):
        types (list[str] | Unset):
        delivery_success (bool | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | EventList]
    """

    kwargs = _get_kwargs(
        type_=type_,
        types=types,
        delivery_success=delivery_success,
        createdgt=createdgt,
        createdgte=createdgte,
        createdlt=createdlt,
        createdlte=createdlte,
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
    type_: str | Unset = UNSET,
    types: list[str] | Unset = UNSET,
    delivery_success: bool | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> ErrorResponse | EventList | None:
    """The account's events in the key's mode, newest first, with Stripe's cursor pagination: the
    notifications webhooks deliver, and the audit log of every key, endpoint, and account change
    with its `actor`. An event stays listed whether or not any endpoint received it.

    Args:
        type_ (str | Unset):
        types (list[str] | Unset):
        delivery_success (bool | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | EventList
    """

    return sync_detailed(
        client=client,
        type_=type_,
        types=types,
        delivery_success=delivery_success,
        createdgt=createdgt,
        createdgte=createdgte,
        createdlt=createdlt,
        createdlte=createdlte,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    type_: str | Unset = UNSET,
    types: list[str] | Unset = UNSET,
    delivery_success: bool | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> Response[ErrorResponse | EventList]:
    """The account's events in the key's mode, newest first, with Stripe's cursor pagination: the
    notifications webhooks deliver, and the audit log of every key, endpoint, and account change
    with its `actor`. An event stays listed whether or not any endpoint received it.

    Args:
        type_ (str | Unset):
        types (list[str] | Unset):
        delivery_success (bool | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | EventList]
    """

    kwargs = _get_kwargs(
        type_=type_,
        types=types,
        delivery_success=delivery_success,
        createdgt=createdgt,
        createdgte=createdgte,
        createdlt=createdlt,
        createdlte=createdlte,
        limit=limit,
        starting_after=starting_after,
        ending_before=ending_before,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    *,
    client: AuthenticatedClient,
    type_: str | Unset = UNSET,
    types: list[str] | Unset = UNSET,
    delivery_success: bool | Unset = UNSET,
    createdgt: int | Unset = UNSET,
    createdgte: int | Unset = UNSET,
    createdlt: int | Unset = UNSET,
    createdlte: int | Unset = UNSET,
    limit: int | Unset = UNSET,
    starting_after: str | Unset = UNSET,
    ending_before: str | Unset = UNSET,
) -> ErrorResponse | EventList | None:
    """The account's events in the key's mode, newest first, with Stripe's cursor pagination: the
    notifications webhooks deliver, and the audit log of every key, endpoint, and account change
    with its `actor`. An event stays listed whether or not any endpoint received it.

    Args:
        type_ (str | Unset):
        types (list[str] | Unset):
        delivery_success (bool | Unset):
        createdgt (int | Unset):
        createdgte (int | Unset):
        createdlt (int | Unset):
        createdlte (int | Unset):
        limit (int | Unset):
        starting_after (str | Unset):
        ending_before (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | EventList
    """

    return (
        await asyncio_detailed(
            client=client,
            type_=type_,
            types=types,
            delivery_success=delivery_success,
            createdgt=createdgt,
            createdgte=createdgte,
            createdlt=createdlt,
            createdlte=createdlte,
            limit=limit,
            starting_after=starting_after,
            ending_before=ending_before,
        )
    ).parsed
