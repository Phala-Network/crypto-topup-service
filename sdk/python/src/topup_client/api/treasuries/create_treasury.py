from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.create_treasury_request import CreateTreasuryRequest
from ...models.error_response import ErrorResponse
from ...models.treasury import Treasury
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    *,
    body: CreateTreasuryRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> dict[str, Any]:
    headers: dict[str, Any] = {}
    if not isinstance(idempotency_key, Unset):
        headers["Idempotency-Key"] = idempotency_key

    _kwargs: dict[str, Any] = {
        "method": "post",
        "url": "/v1/treasuries",
    }

    _kwargs["json"] = body.to_dict()

    headers["Content-Type"] = "application/json"

    _kwargs["headers"] = headers
    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ErrorResponse | Treasury | None:
    if response.status_code == 200:
        response_200 = Treasury.from_dict(response.json())

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

    if response.status_code == 409:
        response_409 = ErrorResponse.from_dict(response.json())

        return response_409

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
) -> Response[ErrorResponse | Treasury]:
    return Response(
        status_code=HTTPStatus(response.status_code),
        content=response.content,
        headers=response.headers,
        parsed=_parse_response(client=client, response=response),
    )


def sync_detailed(
    *,
    client: AuthenticatedClient,
    body: CreateTreasuryRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | Treasury]:
    """Sets the chain's treasury with a signed challenge. An EOA's `personal_sign` signature must
    recover to the address; otherwise a contract deployed at the address must return `0x1626ba7e`
    from EIP-1271 `isValidSignature` for the message's EIP-191 hash at the chain's `finalized`
    block on both of the service's RPC providers. The address is screened against sanctions lists.

     The chain's first treasury, and any test-mode change, applies at once. A later live change is
    `pending` for 48 hours, then applies (`treasury.updated`) unless canceled first: new quotes and
    deposit address networks then pay it, while addresses issued before keep paying the former
    treasury, which becomes `replaced` (`treasury.updated`), and are still credited. Every new
    treasury is announced as `treasury.created`; treasury events go to every enabled webhook
    endpoint of the mode.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryRequest): `POST /v1/treasuries` body. Example: {'chain_id': 1,
            'message': 'pay-api.phala.com wants you to sign in with your Ethereum
            account:\\n0x936c1991f8dA9a919fa11b557a3514719f5A4504\\n\\nSet this address as the test
            mode treasury of acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 on Phala Pay.\\n\\nURI:
            https://pay-api.phala.com\\nVersion: 1\\nChain ID: 1\\nNonce: Kq3nV8xZt2mP6wRa\\nIssued
            At: 2026-09-28T12:00:00Z\\nExpiration Time: 2026-09-28T12:10:00Z', 'signature': '0x5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e1b'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | Treasury]
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
    body: CreateTreasuryRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | Treasury | None:
    """Sets the chain's treasury with a signed challenge. An EOA's `personal_sign` signature must
    recover to the address; otherwise a contract deployed at the address must return `0x1626ba7e`
    from EIP-1271 `isValidSignature` for the message's EIP-191 hash at the chain's `finalized`
    block on both of the service's RPC providers. The address is screened against sanctions lists.

     The chain's first treasury, and any test-mode change, applies at once. A later live change is
    `pending` for 48 hours, then applies (`treasury.updated`) unless canceled first: new quotes and
    deposit address networks then pay it, while addresses issued before keep paying the former
    treasury, which becomes `replaced` (`treasury.updated`), and are still credited. Every new
    treasury is announced as `treasury.created`; treasury events go to every enabled webhook
    endpoint of the mode.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryRequest): `POST /v1/treasuries` body. Example: {'chain_id': 1,
            'message': 'pay-api.phala.com wants you to sign in with your Ethereum
            account:\\n0x936c1991f8dA9a919fa11b557a3514719f5A4504\\n\\nSet this address as the test
            mode treasury of acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 on Phala Pay.\\n\\nURI:
            https://pay-api.phala.com\\nVersion: 1\\nChain ID: 1\\nNonce: Kq3nV8xZt2mP6wRa\\nIssued
            At: 2026-09-28T12:00:00Z\\nExpiration Time: 2026-09-28T12:10:00Z', 'signature': '0x5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e1b'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | Treasury
    """

    return sync_detailed(
        client=client,
        body=body,
        idempotency_key=idempotency_key,
    ).parsed


async def asyncio_detailed(
    *,
    client: AuthenticatedClient,
    body: CreateTreasuryRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> Response[ErrorResponse | Treasury]:
    """Sets the chain's treasury with a signed challenge. An EOA's `personal_sign` signature must
    recover to the address; otherwise a contract deployed at the address must return `0x1626ba7e`
    from EIP-1271 `isValidSignature` for the message's EIP-191 hash at the chain's `finalized`
    block on both of the service's RPC providers. The address is screened against sanctions lists.

     The chain's first treasury, and any test-mode change, applies at once. A later live change is
    `pending` for 48 hours, then applies (`treasury.updated`) unless canceled first: new quotes and
    deposit address networks then pay it, while addresses issued before keep paying the former
    treasury, which becomes `replaced` (`treasury.updated`), and are still credited. Every new
    treasury is announced as `treasury.created`; treasury events go to every enabled webhook
    endpoint of the mode.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryRequest): `POST /v1/treasuries` body. Example: {'chain_id': 1,
            'message': 'pay-api.phala.com wants you to sign in with your Ethereum
            account:\\n0x936c1991f8dA9a919fa11b557a3514719f5A4504\\n\\nSet this address as the test
            mode treasury of acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 on Phala Pay.\\n\\nURI:
            https://pay-api.phala.com\\nVersion: 1\\nChain ID: 1\\nNonce: Kq3nV8xZt2mP6wRa\\nIssued
            At: 2026-09-28T12:00:00Z\\nExpiration Time: 2026-09-28T12:10:00Z', 'signature': '0x5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e1b'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ErrorResponse | Treasury]
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
    body: CreateTreasuryRequest,
    idempotency_key: None | str | Unset = UNSET,
) -> ErrorResponse | Treasury | None:
    """Sets the chain's treasury with a signed challenge. An EOA's `personal_sign` signature must
    recover to the address; otherwise a contract deployed at the address must return `0x1626ba7e`
    from EIP-1271 `isValidSignature` for the message's EIP-191 hash at the chain's `finalized`
    block on both of the service's RPC providers. The address is screened against sanctions lists.

     The chain's first treasury, and any test-mode change, applies at once. A later live change is
    `pending` for 48 hours, then applies (`treasury.updated`) unless canceled first: new quotes and
    deposit address networks then pay it, while addresses issued before keep paying the former
    treasury, which becomes `replaced` (`treasury.updated`), and are still credited. Every new
    treasury is announced as `treasury.created`; treasury events go to every enabled webhook
    endpoint of the mode.

    Args:
        idempotency_key (None | str | Unset):
        body (CreateTreasuryRequest): `POST /v1/treasuries` body. Example: {'chain_id': 1,
            'message': 'pay-api.phala.com wants you to sign in with your Ethereum
            account:\\n0x936c1991f8dA9a919fa11b557a3514719f5A4504\\n\\nSet this address as the test
            mode treasury of acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 on Phala Pay.\\n\\nURI:
            https://pay-api.phala.com\\nVersion: 1\\nChain ID: 1\\nNonce: Kq3nV8xZt2mP6wRa\\nIssued
            At: 2026-09-28T12:00:00Z\\nExpiration Time: 2026-09-28T12:10:00Z', 'signature': '0x5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5
            e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e1b'}.

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ErrorResponse | Treasury
    """

    return (
        await asyncio_detailed(
            client=client,
            body=body,
            idempotency_key=idempotency_key,
        )
    ).parsed
