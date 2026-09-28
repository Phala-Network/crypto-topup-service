from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.api_key_object import ApiKeyObject
from ...models.error_response import ErrorResponse
from ...models.issue_api_key_request import IssueApiKeyRequest
from typing import cast


def _get_kwargs(
    account: str,
    *,
    body: IssueApiKeyRequest,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/admin/accounts/{account}/api_keys".format(
            account=quote(str(account), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ApiKeyObject | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = ApiKeyObject.from_dict(response.json())

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

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ApiKeyObject | ErrorResponse]:
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
    body: IssueApiKeyRequest,
) -> Response[ApiKeyObject | ErrorResponse]:
    """Issues a recovery key (design D7) after the operator verified the request with the recorded
    contact, optionally revoking every key of the mode first. Audited, and announced as
    `api_key.*` events with actor `admin`.

    Args:
        account (str):
        body (IssueApiKeyRequest): `POST /v1/admin/accounts/{account}/api_keys` body: a recovery
            key (design D7).

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ApiKeyObject | ErrorResponse]
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
    body: IssueApiKeyRequest,
) -> ApiKeyObject | ErrorResponse | None:
    """Issues a recovery key (design D7) after the operator verified the request with the recorded
    contact, optionally revoking every key of the mode first. Audited, and announced as
    `api_key.*` events with actor `admin`.

    Args:
        account (str):
        body (IssueApiKeyRequest): `POST /v1/admin/accounts/{account}/api_keys` body: a recovery
            key (design D7).

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ApiKeyObject | ErrorResponse
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
    body: IssueApiKeyRequest,
) -> Response[ApiKeyObject | ErrorResponse]:
    """Issues a recovery key (design D7) after the operator verified the request with the recorded
    contact, optionally revoking every key of the mode first. Audited, and announced as
    `api_key.*` events with actor `admin`.

    Args:
        account (str):
        body (IssueApiKeyRequest): `POST /v1/admin/accounts/{account}/api_keys` body: a recovery
            key (design D7).

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ApiKeyObject | ErrorResponse]
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
    body: IssueApiKeyRequest,
) -> ApiKeyObject | ErrorResponse | None:
    """Issues a recovery key (design D7) after the operator verified the request with the recorded
    contact, optionally revoking every key of the mode first. Audited, and announced as
    `api_key.*` events with actor `admin`.

    Args:
        account (str):
        body (IssueApiKeyRequest): `POST /v1/admin/accounts/{account}/api_keys` body: a recovery
            key (design D7).

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ApiKeyObject | ErrorResponse
    """

    return (
        await asyncio_detailed(
            account=account,
            client=client,
            body=body,
        )
    ).parsed
