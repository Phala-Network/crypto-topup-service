from http import HTTPStatus
from typing import Any, cast
from urllib.parse import quote

import httpx

from ...client import AuthenticatedClient, Client
from ...types import Response, UNSET
from ... import errors

from ...models.client_quote import ClientQuote
from ...models.error_response import ErrorResponse
from ...models.quote import Quote
from ...types import UNSET, Unset
from typing import cast


def _get_kwargs(
    id: str,
    *,
    expand: list[str] | Unset = UNSET,
    client_secret: str | Unset = UNSET,
) -> dict[str, Any]:

    params: dict[str, Any] = {}

    json_expand: list[str] | Unset = UNSET
    if not isinstance(expand, Unset):
        json_expand = expand

    params["expand[]"] = json_expand

    params["client_secret"] = client_secret

    params = {k: v for k, v in params.items() if v is not UNSET and v is not None}

    _kwargs: dict[str, Any] = {
        "method": "get",
        "url": "/v1/quotes/{id}".format(
            id=quote(str(id), safe=""),
        ),
        "params": params,
    }

    return _kwargs


def _parse_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> ClientQuote | Quote | ErrorResponse | None:
    if response.status_code == 200:

        def _parse_response_200(data: object) -> ClientQuote | Quote:
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_quote_view_type_0 = Quote.from_dict(data)

                return componentsschemas_quote_view_type_0
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            if not isinstance(data, dict):
                raise TypeError()
            componentsschemas_quote_view_type_1 = ClientQuote.from_dict(data)

            return componentsschemas_quote_view_type_1

        response_200 = _parse_response_200(response.json())

        return response_200

    if response.status_code == 401:
        response_401 = ErrorResponse.from_dict(response.json())

        return response_401

    if response.status_code == 404:
        response_404 = ErrorResponse.from_dict(response.json())

        return response_404

    if response.status_code == 429:
        response_429 = ErrorResponse.from_dict(response.json())

        return response_429

    if client.raise_on_unexpected_status:
        raise errors.UnexpectedStatus(response.status_code, response.content)
    else:
        return None


def _build_response(
    *, client: AuthenticatedClient | Client, response: httpx.Response
) -> Response[ClientQuote | Quote | ErrorResponse]:
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
    expand: list[str] | Unset = UNSET,
    client_secret: str | Unset = UNSET,
) -> Response[ClientQuote | Quote | ErrorResponse]:
    """One quote, for example to resume a checkout page. The payer's browser can read the quote's
    public view with its `client_secret` instead of an API key, as Stripe.js reads a PaymentIntent.

    Args:
        id (str):
        expand (list[str] | Unset):
        client_secret (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ClientQuote | Quote | ErrorResponse]
    """

    kwargs = _get_kwargs(
        id=id,
        expand=expand,
        client_secret=client_secret,
    )

    response = client.get_httpx_client().request(
        **kwargs,
    )

    return _build_response(client=client, response=response)


def sync(
    id: str,
    *,
    client: AuthenticatedClient,
    expand: list[str] | Unset = UNSET,
    client_secret: str | Unset = UNSET,
) -> ClientQuote | Quote | ErrorResponse | None:
    """One quote, for example to resume a checkout page. The payer's browser can read the quote's
    public view with its `client_secret` instead of an API key, as Stripe.js reads a PaymentIntent.

    Args:
        id (str):
        expand (list[str] | Unset):
        client_secret (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ClientQuote | Quote | ErrorResponse
    """

    return sync_detailed(
        id=id,
        client=client,
        expand=expand,
        client_secret=client_secret,
    ).parsed


async def asyncio_detailed(
    id: str,
    *,
    client: AuthenticatedClient,
    expand: list[str] | Unset = UNSET,
    client_secret: str | Unset = UNSET,
) -> Response[ClientQuote | Quote | ErrorResponse]:
    """One quote, for example to resume a checkout page. The payer's browser can read the quote's
    public view with its `client_secret` instead of an API key, as Stripe.js reads a PaymentIntent.

    Args:
        id (str):
        expand (list[str] | Unset):
        client_secret (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        Response[ClientQuote | Quote | ErrorResponse]
    """

    kwargs = _get_kwargs(
        id=id,
        expand=expand,
        client_secret=client_secret,
    )

    response = await client.get_async_httpx_client().request(**kwargs)

    return _build_response(client=client, response=response)


async def asyncio(
    id: str,
    *,
    client: AuthenticatedClient,
    expand: list[str] | Unset = UNSET,
    client_secret: str | Unset = UNSET,
) -> ClientQuote | Quote | ErrorResponse | None:
    """One quote, for example to resume a checkout page. The payer's browser can read the quote's
    public view with its `client_secret` instead of an API key, as Stripe.js reads a PaymentIntent.

    Args:
        id (str):
        expand (list[str] | Unset):
        client_secret (str | Unset):

    Raises:
        errors.UnexpectedStatus: If the server returns an undocumented status code and Client.raise_on_unexpected_status is True.
        httpx.TimeoutException: If the request takes longer than Client.timeout.

    Returns:
        ClientQuote | Quote | ErrorResponse
    """

    return (
        await asyncio_detailed(
            id=id,
            client=client,
            expand=expand,
            client_secret=client_secret,
        )
    ).parsed
