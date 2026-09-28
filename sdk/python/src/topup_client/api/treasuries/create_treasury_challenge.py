from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.create_treasury_challenge_request import CreateTreasuryChallengeRequest
from ...models.error_response import ErrorResponse
from ...models.treasury_challenge import TreasuryChallenge
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    body: CreateTreasuryChallengeRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/treasuries/challenge",
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | TreasuryChallenge | None:
    if response.status_code == 200:
        response_200 = TreasuryChallenge.from_dict(response.json())

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

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ErrorResponse | TreasuryChallenge]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    body: CreateTreasuryChallengeRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | TreasuryChallenge]:
    """Issues the EIP-4361 message that proves `address` as your treasury on `chain_id` in the key's
    mode. Sign it and send it to `POST /v1/treasuries` before `expires_at`: 10 minutes for an EOA,
    24 hours for an address that holds code (a Safe, whose owners sign it as a Safe message); it
    can be used once.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryChallengeRequest): `POST /v1/treasuries/challenge` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | TreasuryChallenge]
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
    body: CreateTreasuryChallengeRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | TreasuryChallenge | None:
    """Issues the EIP-4361 message that proves `address` as your treasury on `chain_id` in the key's
    mode. Sign it and send it to `POST /v1/treasuries` before `expires_at`: 10 minutes for an EOA,
    24 hours for an address that holds code (a Safe, whose owners sign it as a Safe message); it
    can be used once.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryChallengeRequest): `POST /v1/treasuries/challenge` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | TreasuryChallenge
    """

    return sync_detailed(
        client=client,
        body=body,
        idempotency_key=idempotency_key,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    body: CreateTreasuryChallengeRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | TreasuryChallenge]:
    """Issues the EIP-4361 message that proves `address` as your treasury on `chain_id` in the key's
    mode. Sign it and send it to `POST /v1/treasuries` before `expires_at`: 10 minutes for an EOA,
    24 hours for an address that holds code (a Safe, whose owners sign it as a Safe message); it
    can be used once.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryChallengeRequest): `POST /v1/treasuries/challenge` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | TreasuryChallenge]
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
    body: CreateTreasuryChallengeRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | TreasuryChallenge | None:
    """Issues the EIP-4361 message that proves `address` as your treasury on `chain_id` in the key's
    mode. Sign it and send it to `POST /v1/treasuries` before `expires_at`: 10 minutes for an EOA,
    24 hours for an address that holds code (a Safe, whose owners sign it as a Safe message); it
    can be used once.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryChallengeRequest): `POST /v1/treasuries/challenge` body.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | TreasuryChallenge
    """

    return (
        await asyncio_detailed(
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
