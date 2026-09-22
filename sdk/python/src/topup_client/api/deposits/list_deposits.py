from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.deposits_response import DepositsResponse
from ...models.error_response import ErrorResponse
from ...types import UNSET, Unset
from typing import cast
from uuid import UUID
import datetime


def _get_kwargs(
    p: str,
    ext: str,
    *,
    state: str | Unset = UNSET,
    from_: datetime.datetime | Unset = UNSET,
    to: datetime.datetime | Unset = UNSET,
    cursor: UUID | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    params["state"] = state

    json_from_: str | Unset = UNSET
    if not isinstance(from_, Unset):
        json_from_ = from_.isoformat()
    params["from"] = json_from_

    json_to: str | Unset = UNSET
    if not isinstance(to, Unset):
        json_to = to.isoformat()
    params["to"] = json_to

    json_cursor: str | Unset = UNSET
    if not isinstance(cursor, Unset):
        json_cursor = str(cursor)
    params["cursor"] = json_cursor

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/products/{p}/accounts/{ext}/deposits".format(
            p=quote(str(p), safe=""),
            ext=quote(str(ext), safe=""),
        ),
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> DepositsResponse | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = DepositsResponse.from_dict(response.json())

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
) -> Response[DepositsResponse | ErrorResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    p: str,
    ext: str,
    *,
    client: AuthenticatedClient,
    state: str | Unset = UNSET,
    from_: datetime.datetime | Unset = UNSET,
    to: datetime.datetime | Unset = UNSET,
    cursor: UUID | Unset = UNSET,
) -> Response[DepositsResponse | ErrorResponse]:
    """
    Args:
        p (str):
        ext (str):
        state (str | Unset):
        from_ (datetime.datetime | Unset):
        to (datetime.datetime | Unset):
        cursor (UUID | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[DepositsResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        ext=ext,
        state=state,
        from_=from_,
        to=to,
        cursor=cursor,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    p: str,
    ext: str,
    *,
    client: AuthenticatedClient,
    state: str | Unset = UNSET,
    from_: datetime.datetime | Unset = UNSET,
    to: datetime.datetime | Unset = UNSET,
    cursor: UUID | Unset = UNSET,
) -> DepositsResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        ext (str):
        state (str | Unset):
        from_ (datetime.datetime | Unset):
        to (datetime.datetime | Unset):
        cursor (UUID | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        DepositsResponse | ErrorResponse
    """

    return sync_detailed(
        p=p,
        ext=ext,
        client=client,
        state=state,
        from_=from_,
        to=to,
        cursor=cursor,
    ).parsed


async def asyncio_detailed(
    p: str,
    ext: str,
    *,
    client: AuthenticatedClient,
    state: str | Unset = UNSET,
    from_: datetime.datetime | Unset = UNSET,
    to: datetime.datetime | Unset = UNSET,
    cursor: UUID | Unset = UNSET,
) -> Response[DepositsResponse | ErrorResponse]:
    """
    Args:
        p (str):
        ext (str):
        state (str | Unset):
        from_ (datetime.datetime | Unset):
        to (datetime.datetime | Unset):
        cursor (UUID | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[DepositsResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        p=p,
        ext=ext,
        state=state,
        from_=from_,
        to=to,
        cursor=cursor,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    p: str,
    ext: str,
    *,
    client: AuthenticatedClient,
    state: str | Unset = UNSET,
    from_: datetime.datetime | Unset = UNSET,
    to: datetime.datetime | Unset = UNSET,
    cursor: UUID | Unset = UNSET,
) -> DepositsResponse | ErrorResponse | None:
    """
    Args:
        p (str):
        ext (str):
        state (str | Unset):
        from_ (datetime.datetime | Unset):
        to (datetime.datetime | Unset):
        cursor (UUID | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        DepositsResponse | ErrorResponse
    """

    return (
        await asyncio_detailed(
            p=p,
            ext=ext,
            client=client,
            state=state,
            from_=from_,
            to=to,
            cursor=cursor,
        )
    ).parsed
