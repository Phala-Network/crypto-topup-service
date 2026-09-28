from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.deposit import Deposit
from ...models.error_response import ErrorResponse
from ...models.update_metadata_request import UpdateMetadataRequest
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    id: str,
    *,
    body: UpdateMetadataRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/deposits/{id}".format(
            id=quote(str(id), safe=""),
        ),
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Deposit | ErrorResponse | None:
    if response.status_code == 200:
        response_200 = Deposit.from_dict(response.json())

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
) -> Response[Deposit | ErrorResponse]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateMetadataRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[Deposit | ErrorResponse]:
    """Updates a deposit's `metadata`; parameters not sent are left unchanged. The quote's metadata is
    not changed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateMetadataRequest): `POST /v1/quotes/{id}`, `POST /v1/deposits/{id}`, `POST
            /v1/refunds/{id}`, and
            `POST /v1/deposit_addresses/{id}` body: the
            object's updatable parameters, of which `metadata` is the one. Example: {'metadata':
            {'note': '', 'order_id': 'ord_1001'}}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[Deposit | ErrorResponse]
    """

    kwargs = _get_kwargs(
        id=id,
        body=body,
        idempotency_key=idempotency_key,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateMetadataRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Deposit | ErrorResponse | None:
    """Updates a deposit's `metadata`; parameters not sent are left unchanged. The quote's metadata is
    not changed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateMetadataRequest): `POST /v1/quotes/{id}`, `POST /v1/deposits/{id}`, `POST
            /v1/refunds/{id}`, and
            `POST /v1/deposit_addresses/{id}` body: the
            object's updatable parameters, of which `metadata` is the one. Example: {'metadata':
            {'note': '', 'order_id': 'ord_1001'}}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Deposit | ErrorResponse
    """

    return sync_detailed(
        id=id,
        client=client,
        body=body,
        idempotency_key=idempotency_key,
    ).parsed


async def asyncio_detailed(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateMetadataRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[Deposit | ErrorResponse]:
    """Updates a deposit's `metadata`; parameters not sent are left unchanged. The quote's metadata is
    not changed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateMetadataRequest): `POST /v1/quotes/{id}`, `POST /v1/deposits/{id}`, `POST
            /v1/refunds/{id}`, and
            `POST /v1/deposit_addresses/{id}` body: the
            object's updatable parameters, of which `metadata` is the one. Example: {'metadata':
            {'note': '', 'order_id': 'ord_1001'}}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[Deposit | ErrorResponse]
    """

    kwargs = _get_kwargs(
        id=id,
        body=body,
        idempotency_key=idempotency_key,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    id: str,
    *,
    client: AuthenticatedClient,
    body: UpdateMetadataRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Deposit | ErrorResponse | None:
    """Updates a deposit's `metadata`; parameters not sent are left unchanged. The quote's metadata is
    not changed.

    Args:
        id (str):
        idempotency_key (None | str | Unset):
        body (UpdateMetadataRequest): `POST /v1/quotes/{id}`, `POST /v1/deposits/{id}`, `POST
            /v1/refunds/{id}`, and
            `POST /v1/deposit_addresses/{id}` body: the
            object's updatable parameters, of which `metadata` is the one. Example: {'metadata':
            {'note': '', 'order_id': 'ord_1001'}}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Deposit | ErrorResponse
    """

    return (
        await asyncio_detailed(
            id=id,
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
