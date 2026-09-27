# phala-pay

Phala Pay for Python (3.12+), in the shape of Stripe's SDK: create a
quote, hand its `client_secret` to the browser checkout (`@phala/pay`), and fulfil from
the signed `deposit.credited` webhook.

## Install

```sh
uv add phala-pay        # or: pip install phala-pay
```

Until the first PyPI release, install from the repository (read access is required):

```sh
uv add "phala-pay @ git+https://github.com/Phala-Network/phala-pay#subdirectory=sdk/python"
```

## Quickstart

Create the product key once and send only the printed public key to the operator:

```sh
uvx --from phala-pay topup-sdk keygen --keyid acme/v1 --seed-out product.seed
```

Create a quote for the signed-in account and return its client secret to the browser:

```python
from phala_pay import PhalaPay

pay = PhalaPay(api_base="https://pay.example.com", key_id="acme/v1", key_file="product.seed")

quote = pay.quotes.create(
    account_id="team-42",  # your id for the customer; credits are addressed to it
    amount=2500,  # US cents
    chain_id=11155111,
    asset="pha",
    idempotency_key=order_id,  # the same key returns the same quote, with a fresh client secret
)
return {"client_secret": quote.client_secret}
```

Fulfil from the webhook, once per deposit, and answer `2xx` after the credit is committed:

```python
from phala_pay import SignatureVerificationError

try:
    event = pay.webhooks.construct_event(raw_body, request.headers, SETTLEMENT_PUBLIC_KEY)
except (SignatureVerificationError, ValueError):
    return Response(status_code=400)

if event.type == "deposit.credited":
    deposit = event.deposit
    credit_once(key=deposit.id, account=deposit.account_id, cents=deposit.amount)
```

`SETTLEMENT_PUBLIC_KEY` is the service's webhook key, pinned from its attestation
(docs/integration.md §3.3). `construct_event` checks the Standard Webhooks
signature, the timestamp (five minutes' tolerance), and that the body's id is the `webhook-id`;
`event.data.object` is the `Deposit` (or, for `quote.expired`, the `Quote`) as it was when the
event happened. `sdk/examples/fastapi_app.py` is a complete FastAPI backend with both routes.

## Reference

| Call | API |
|---|---|
| `pay.quotes.create(account_id=, amount=, chain_id=, asset=, idempotency_key=)` | `POST /v1/quotes` |
| `pay.quotes.retrieve(id)` / `.cancel(id)` | `GET /v1/quotes/{id}`, `POST /v1/quotes/{id}/cancel` |
| `pay.deposits.list(account_id=, quote=, status=, tx_hash=, created_gte=, created_lte=)` | `GET /v1/deposits`, every page |
| `pay.deposits.retrieve(id)` | `GET /v1/deposits/{id}` |
| `pay.refunds.create(deposit=, destination_address=, amount_atomic=)` / `.retrieve(id)` | `POST /v1/refunds`, `GET /v1/refunds/{id}` |
| `pay.config.retrieve()` | `GET /v1/config` |
| `pay.webhooks.construct_event(payload, headers, public_key)` (also `phala_pay.Webhook`, no client needed) | verifies a webhook delivery |

Every request is signed with the product key (RFC 9421). Transport errors, `429`, and `5xx` are
retried with backoff, reusing one `Idempotency-Key` per `POST`. Failures raise `ApiError` with the
service's stable `code`, `error_type`, and `param`. With `forwarder=(factory, implementation)`
pinned from the attested deployment, `quotes.create` and `quotes.retrieve` also recompute the
deposit address and raise `AddressMismatchError` on a difference.

Lower-level modules: `topup_sdk` (request signing and verification, address derivation,
attestation, `TopupClient`) and `topup_client` (generated from `crates/topup/openapi.json`; do not
edit). `uv run topup-sdk send-test-event --url … --seed-file test.seed --account-id …` sends a
signed test event, a duplicate, and a forged copy to a webhook receiver whose test instance pins
that seed's public key. See `docs/integration.md` for the integration guide and the versioning
and deprecation policy, `deploy/product/reference_product` for a complete product, and
`deploy/sandbox/README.md` for the sandbox.

## Development

```sh
make sync    # install the locked environment
make check   # ruff, mypy --strict, pytest, and the regeneration no-op check
```

A tag `sdk-py-v<version>` matching `pyproject.toml` publishes to PyPI from the `release` workflow
with trusted publishing.
