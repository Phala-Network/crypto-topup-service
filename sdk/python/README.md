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

pay = PhalaPay(
    api_base="https://pay.example.com",
    api_key=os.environ["PHALA_PAY_SECRET_KEY"],
    # The forwarder factory and implementation, pinned from the attested deployment: every quote
    # and deposit address is recomputed from them, failing closed (AddressMismatchError).
    forwarder=(FACTORY, IMPLEMENTATION),
)

quote = pay.quotes.create(
    client_reference_id="team-42",  # your id for the customer; credits are addressed to it
    amount=2500,  # US cents
    chain_id=11155111,
    asset="pha",
    idempotency_key=order_id,  # for 24 hours the same key replays this quote and client secret
)
# The page renders <Checkout clientSecret expectedAddress apiBase /> (@phala/pay).
return {"client_secret": quote.client_secret, "expected_address": quote.address}
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
    credit_once(key=deposit.id, customer=deposit.client_reference_id, cents=deposit.amount)
elif event.type in ("deposit.reversed", "deposit.refunded"):
    claw_back_once(key=event.id, deposit=event.deposit.id)
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
| `pay.account.retrieve()` / `.update(confirmation_policies=)` / `.pause_quotes()` / `.resume_quotes()` / `.roll_webhook_key(expires_in=)` | `GET\|POST /v1/account`, `POST /v1/account/pause\|resume`, `POST /v1/account/webhook_keys/roll` |
| `pay.quotes.create(client_reference_id=, amount=, chain_id=, asset=, idempotency_key=, metadata=)` | `POST /v1/quotes` |
| `pay.quotes.retrieve(id)` / `.list(client_reference_id=, status=)` / `.cancel(id)` | `GET /v1/quotes[/{id}]`, `POST /v1/quotes/{id}/cancel` |
| `pay.quotes.update(id, metadata=)` | `POST /v1/quotes/{id}` |
| `pay.deposit_addresses.create(client_reference_id=, metadata=)` | `POST /v1/deposit_addresses`: the customer's active address, one for every supported token and network (`networks`), its recent `payments`, and a `client_secret` for `<DepositAddress>` |
| `pay.deposit_addresses.retrieve(id)` / `.list(client_reference_id=, status=)` / `.rotate(id)` / `.update(id, metadata=)` | `GET /v1/deposit_addresses[/{id}]`, `POST /v1/deposit_addresses/{id}/rotate`, `POST /v1/deposit_addresses/{id}` |
| `pay.deposits.list(client_reference_id=, quote=, deposit_address=, status=, tx_hash=, created_gte=, created_lte=)` | `GET /v1/deposits`, every page; `status` is `pending`, `credited`, `rejected`, or `reversed` |
| `pay.deposits.retrieve(id)` / `.update(id, metadata=)` | `GET /v1/deposits/{id}`, `POST /v1/deposits/{id}` |
| `pay.refunds.create(deposit=, destination_address=, amount_atomic=, metadata=)` / `.mark_paid(id, transaction_hash=, log_index=)` / `.cancel(id)` / `.retrieve(id)` / `.update(id, metadata=)` | `POST /v1/refunds`, `POST /v1/refunds/{id}/mark_paid`, `POST /v1/refunds/{id}/cancel`, `GET /v1/refunds/{id}`, `POST /v1/refunds/{id}` |
| `pay.refunds.list(deposit=, status=)` | `GET /v1/refunds`, every page |
| `pay.config.retrieve()` | `GET /v1/config` |
| `pay.balance.retrieve()` / `pay.sweeps.list(chain_id=, forwarder=, token=)` / `pay.forwarders.list(chain_id=, sweepable=)` | `GET /v1/balance`, `GET /v1/sweeps`, `GET /v1/forwarders` |
| `pay.treasuries.challenge(chain_id=, address=)` / `.create(chain_id=, message=, signature=)` / `.set_eoa(chain_id=, address=, private_key=)` / `.list()` / `.retrieve(id)` / `.cancel(id)` | `POST /v1/treasuries/challenge`, `GET\|POST /v1/treasuries`, `POST /v1/treasuries/{id}/cancel` |
| `pay.api_keys.create(name=)` / `.list()` / `.retrieve(id)` / `.roll(id, expires_in=)` / `.revoke(id)` | `/v1/api_keys` |
| `pay.webhook_endpoints.create(url=, enabled_events=)` / `.list()` / `.retrieve(id)` / `.update(id, …)` / `.delete(id)` / `.test(id)` | `/v1/webhook_endpoints` |
| `pay.events.list(type=)` / `.retrieve(id)` / `.resend(id, webhook_endpoint=)` | `/v1/events` |
| `pay.export_account(directory)` | every list, written as JSON files |
| `pay.webhooks.construct_event(payload, headers, public_key, expected_account, expected_livemode=)` (also `phala_pay.Webhook`, no client needed) | verifies a webhook delivery |

Every request sends the secret key as `Authorization: Bearer ppay_sk_…`. Transport errors, `429`,
`5xx`, and `409 idempotency_key_in_use` are retried with backoff, reusing one `Idempotency-Key`
per `POST`. Failures raise `ApiError` with the
service's stable `code`, `error_type`, `param`, and `request_id` (the response's `Request-Id`).
`forwarder=(factory, implementation)`, pinned from the attested deployment, is required: every
open quote is recomputed over its `treasury`, and every network of an active deposit address over
its own, from your account id (read once from `GET /v1/account`, or passed as `account=`); a
difference raises `AddressMismatchError`. `treasuries={chain_id: treasury}` additionally refuses an
address that pays any other treasury, or a chain without a pinned one. `topup_sdk.deposit_address(factory, implementation, treasury, account=, livemode=,
client_reference_id=, version=)` recomputes any deposit address offline; pass the treasury of the
network, since a chain whose treasury differs has its own address.

**Sweeping.** Funds stay in the forwarders until you sweep them, with your own wallet or Safe:

```python
from topup_sdk import flush_transactions, safe_batch, write_safe_batch

forwarders = list(pay.forwarders.list(chain_id=1, sweepable=PHA))  # never a sanctioned one
calls = flush_transactions(forwarders, PHA)  # offline: one factory.flush per treasury
write_safe_batch("sweep.json", safe_batch(1, TREASURY_SAFE, calls))  # for the Transaction Builder
```

`flush_transaction(factory, treasury, salts, token)` encodes one call offline from exported
forwarders (`pay.export_account` writes them all), so funds stay sweepable without the service;
`safe_batch` writes the Safe Transaction Builder's `BatchFile` JSON with its checksum.

**Treasuries.** An EOA proves itself with `pay.treasuries.set_eoa(chain_id=, address=,
private_key=)` (install `phala-pay[eoa]`); a Safe's owners sign the challenge's `message` as a Safe
message with the Safe{Core} SDK and submit it with `pay.treasuries.create` (docs/integration.md
§1.6, "Safe treasuries").

`metadata` follows [Stripe's](https://docs.stripe.com/api/metadata): up to 50 string key/value
pairs, keys of up to 40 characters without square brackets, values of up to 500 characters. An
`update` merges: a key set to `""` is unset, and `metadata=""` unsets every key. A deposit starts
with a copy of its quote's metadata, so an order id set on the quote arrives in the
`deposit.credited` webhook's `data.object.metadata`. Do not store sensitive information in it.

Lower-level modules: `topup_sdk` (webhook and admin request signatures, address derivation,
attestation, `TopupClient`) and `topup_client` (generated from `crates/topup/openapi.json`; do not
edit). `uv run topup-sdk send-test-event --url … --seed-file test.seed --client-reference-id …` sends a
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
