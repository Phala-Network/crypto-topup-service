from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.account_response import AccountResponse
from ...models.error_response import ErrorResponse
from ...models.update_account_request import UpdateAccountRequest
from typing import cast


def _get_kwargs(
    account: str,
    *,
    body: UpdateAccountRequest,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/admin/accounts/{account}".format(
            account=quote(str(account), safe=""),
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

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

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
    account: str,
    *,
    client: AuthenticatedClient,
    body: UpdateAccountRequest,
) -> Response[AccountResponse | ErrorResponse]:
    """Updates an account: live mode (enabling it returns the first live key), the restricted flag,
    the contact, or the webhook URL. Audited, and announced to the account as `account.updated`.

    Args:
        account (str):
        body (UpdateAccountRequest): `POST /v1/admin/accounts/{account}` body; absent fields stay
            as they are.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[AccountResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        account=account,
        body=body,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    account: str,
    *,
    client: AuthenticatedClient,
    body: UpdateAccountRequest,
) -> AccountResponse | ErrorResponse | None:
    """Updates an account: live mode (enabling it returns the first live key), the restricted flag,
    the contact, or the webhook URL. Audited, and announced to the account as `account.updated`.

    Args:
        account (str):
        body (UpdateAccountRequest): `POST /v1/admin/accounts/{account}` body; absent fields stay
            as they are.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        AccountResponse | ErrorResponse
    """

    return sync_detailed(
        account=account,
        client=client,
        body=body,
    ).parsed


async def asyncio_detailed(
    account: str,
    *,
    client: AuthenticatedClient,
    body: UpdateAccountRequest,
) -> Response[AccountResponse | ErrorResponse]:
    """Updates an account: live mode (enabling it returns the first live key), the restricted flag,
    the contact, or the webhook URL. Audited, and announced to the account as `account.updated`.

    Args:
        account (str):
        body (UpdateAccountRequest): `POST /v1/admin/accounts/{account}` body; absent fields stay
            as they are.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[AccountResponse | ErrorResponse]
    """

    kwargs = _get_kwargs(
        account=account,
        body=body,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    account: str,
    *,
    client: AuthenticatedClient,
    body: UpdateAccountRequest,
) -> AccountResponse | ErrorResponse | None:
    """Updates an account: live mode (enabling it returns the first live key), the restricted flag,
    the contact, or the webhook URL. Audited, and announced to the account as `account.updated`.

    Args:
        account (str):
        body (UpdateAccountRequest): `POST /v1/admin/accounts/{account}` body; absent fields stay
            as they are.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        AccountResponse | ErrorResponse
    """

    return (
        await asyncio_detailed(
            account=account,
            client=client,
            body=body,
        )
    ).parsed
