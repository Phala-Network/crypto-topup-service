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

Some resolvers drop the `#subdirectory=` fragment (PDM delegating resolution to uv, for example);
install with `uv` or `pip` directly, and switch to the PyPI package once it is released.

## Quickstart

The operator creates your account and hands your contact its first secret key, `ppay_sk_test_…`;
roll it on receipt and keep the new key in your secret store.

Create a quote for the signed-in account and return its client secret to the browser:

```python
import os

from phala_pay import PhalaPay

pay = PhalaPay(api_base="https://pay.example.com", api_key=os.environ["PHALA_PAY_SECRET_KEY"])

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
    event = pay.webhooks.construct_event(
        raw_body, request.headers, WEBHOOK_PUBLIC_KEY, "acct_…", expected_livemode=False
    )
except (SignatureVerificationError, ValueError):
    return Response(status_code=400)

if event.type == "deposit.credited":
    deposit = event.deposit
    credit_once(key=deposit.id, account=deposit.account_id, cents=deposit.amount)
```

`WEBHOOK_PUBLIC_KEY` is your account's webhook key in the mode, pinned from `GET
/v1/attestation` (docs/integration.md §5.3); pass a list of keys while a rotation overlaps.
`construct_event` fails closed: it checks the Standard Webhooks signature, the timestamp (five
minutes' tolerance), that the body's id is the `webhook-id`, and that the event's `account` and
`livemode` are the expected ones;
`event.data.object` is the `Deposit` (or, for `quote.expired`, the `Quote`) as it was when the
event happened. `sdk/examples/fastapi_app.py` is a complete FastAPI backend with both routes.

## Reference

| Call | API |
|---|---|
| `pay.quotes.create(account_id=, amount=, chain_id=, asset=, idempotency_key=, metadata=)` | `POST /v1/quotes` |
| `pay.quotes.retrieve(id)` / `.cancel(id)` | `GET /v1/quotes/{id}`, `POST /v1/quotes/{id}/cancel` |
| `pay.quotes.update(id, metadata=)` | `POST /v1/quotes/{id}` |
| `pay.deposit_addresses.create(client_reference_id=, metadata=)` | `POST /v1/deposit_addresses`: the customer's active address, one for every supported token and network (`networks`) |
| `pay.deposit_addresses.retrieve(id)` / `.list(client_reference_id=, status=)` / `.rotate(id)` / `.update(id, metadata=)` | `GET /v1/deposit_addresses[/{id}]`, `POST /v1/deposit_addresses/{id}/rotate`, `POST /v1/deposit_addresses/{id}` |
| `pay.deposits.list(account_id=, quote=, deposit_address=, status=, tx_hash=, created_gte=, created_lte=)` | `GET /v1/deposits`, every page |
| `pay.deposits.retrieve(id)` / `.update(id, metadata=)` | `GET /v1/deposits/{id}`, `POST /v1/deposits/{id}` |
| `pay.refunds.create(deposit=, destination_address=, amount_atomic=, metadata=)` / `.mark_paid(id, transaction_hash=, log_index=)` / `.cancel(id)` / `.retrieve(id)` / `.update(id, metadata=)` | `POST /v1/refunds`, `POST /v1/refunds/{id}/mark_paid`, `POST /v1/refunds/{id}/cancel`, `GET /v1/refunds/{id}`, `POST /v1/refunds/{id}` |
| `pay.config.retrieve()` | `GET /v1/config` |
| `pay.webhooks.construct_event(payload, headers, public_key)` (also `phala_pay.Webhook`, no client needed) | verifies a webhook delivery |

Every request sends the secret key as `Authorization: Bearer ppay_sk_…`. Transport errors, `429`,
`5xx`, and `409 idempotency_key_in_use` are retried with backoff, reusing one `Idempotency-Key`
per `POST`. Failures raise `ApiError` with the
service's stable `code`, `error_type`, and `param`. With `forwarder=(factory, implementation,
treasury)` pinned from the attested deployment and your treasury, `quotes.create` and
`quotes.retrieve` also recompute the quote's address, and the `deposit_addresses` calls every
network of an active deposit address, which must pay the pinned treasury (from your account id,
read once from `GET /v1/account` or passed as `account=`), and raise `AddressMismatchError` on a
difference. `topup_sdk.deposit_address(factory, implementation, treasury, account=, livemode=,
client_reference_id=, version=)` recomputes any deposit address offline; pass the treasury of the
network, since a chain whose treasury differs has its own address.

`metadata` follows [Stripe's](https://docs.stripe.com/api/metadata): up to 50 string key/value
pairs, keys of up to 40 characters without square brackets, values of up to 500 characters. An
`update` merges: a key set to `""` is unset, and `metadata=""` unsets every key. A deposit starts
with a copy of its quote's metadata, so an order id set on the quote arrives in the
`deposit.credited` webhook's `data.object.metadata`. Do not store sensitive information in it.

Lower-level modules: `topup_sdk` (webhook and admin request signatures, address derivation,
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

A tag `sdk-py-v<version>` matching `pyproject.toml` publishes to PyPI from the Release SDKs workflow
(environment `pypi`) with trusted publishing.
