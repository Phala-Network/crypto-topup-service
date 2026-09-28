from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.account_object import AccountObject
from ...models.account_self_pause_request import AccountSelfPauseRequest
from ...models.error_response import ErrorResponse
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    body: AccountSelfPauseRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/account/resume",
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> AccountObject | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = AccountObject.from_dict(response.json())

        return response_200

    if response.status_code == 400:
        response_400 = ErrorResponse.from_dict(response.json())

        return response_400

    if response.status_code == 401:
        response_401 = ErrorResponse.from_dict(response.json())

        return response_401

    if response.status_code == 409:
        response_409 = ErrorResponse.from_dict(response.json())

        return response_409

    if response.status_code == 429:
        response_429 = ErrorResponse.from_dict(response.json())

        return response_429

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[AccountObject | ErrorResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    body: AccountSelfPauseRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[AccountObject | ErrorResponse]:
    """Resumes the `quotes` you paused. A pause the operator set stays in `paused_scopes` until the
    operator lifts it. Announced as `account.updated`.

    Args:
        idempotency_key (None | str | Unset):
        body (AccountSelfPauseRequest): `POST /v1/account/pause` and `POST /v1/account/resume`
            body. Example: {'scopes': ['quotes']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[AccountObject | ErrorResponse]
    """

    kwargs = _get_kwargs(
        body=body,
        idempotency_key=idempotency_key,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    *,
    client: AuthenticatedClient,
    body: AccountSelfPauseRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> AccountObject | ErrorResponse | None:
    """Resumes the `quotes` you paused. A pause the operator set stays in `paused_scopes` until the
    operator lifts it. Announced as `account.updated`.

    Args:
        idempotency_key (None | str | Unset):
        body (AccountSelfPauseRequest): `POST /v1/account/pause` and `POST /v1/account/resume`
            body. Example: {'scopes': ['quotes']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        AccountObject | ErrorResponse
    """

    return sync_detailed(
        client=client,
        body=body,
        idempotency_key=idempotency_key,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    body: AccountSelfPauseRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[AccountObject | ErrorResponse]:
    """Resumes the `quotes` you paused. A pause the operator set stays in `paused_scopes` until the
    operator lifts it. Announced as `account.updated`.

    Args:
        idempotency_key (None | str | Unset):
        body (AccountSelfPauseRequest): `POST /v1/account/pause` and `POST /v1/account/resume`
            body. Example: {'scopes': ['quotes']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[AccountObject | ErrorResponse]
    """

    kwargs = _get_kwargs(
        body=body,
        idempotency_key=idempotency_key,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    *,
    client: AuthenticatedClient,
    body: AccountSelfPauseRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> AccountObject | ErrorResponse | None:
    """Resumes the `quotes` you paused. A pause the operator set stays in `paused_scopes` until the
    operator lifts it. Announced as `account.updated`.

    Args:
        idempotency_key (None | str | Unset):
        body (AccountSelfPauseRequest): `POST /v1/account/pause` and `POST /v1/account/resume`
            body. Example: {'scopes': ['quotes']}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        AccountObject | ErrorResponse
    """

    return (
        await asyncio_detailed(
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
