# crypto-topup-sdk

Python SDK for the crypto top-up service product API (Python 3.12+).

- `topup_client`: generated from `crates/topup/openapi.json`; do not edit.
- `topup_sdk`: request signing, request and webhook verification, typed
  `deposit.credited` credits for fulfillment (`CreditedDeposit`), address and deposit-id
  recomputation, and `TopupClient`, whose helpers are idempotent and retry safely.

```python
from topup_sdk import RequestSigner, TopupClient

signer = RequestSigner.from_seed_file("acme/v1", "product.seed")  # the key id names product acme
forwarder = ("0x<factory>", "0x<implementation>")  # pinned from the attested deployment
with TopupClient("https://topup.example", signer, forwarder=forwarder) as client:
    quote = client.create_quote("workspace-42", 2500, chain_id=1, asset="pha")
    print(quote.id, quote.address, quote.amount_atomic, quote.expires_at)
```

Create a product key with `uv run topup-sdk keygen --keyid acme/v1 --seed-out product.seed` and
send only the printed public key to the operator.
`uv run topup-sdk send-test-event --url … --seed-file test.seed --account-id …` delivers a
signed test `deposit.credited` to a webhook receiver whose test instance pins that seed's public
key, then a duplicate and a forged copy, and reports whether the answers were `2xx`, `2xx`, and
`4xx`. See `docs/integration.md` for the integration guide, the signing profile, and the versioning and deprecation policy,
`sdk/examples/phala_cloud_integration.py` for an integration including webhook
fulfillment, `deploy/product/reference_product` for a complete product, and
`deploy/sandbox/README.md` for the sandbox.

```sh
make sync    # install the locked environment
make check   # ruff, mypy --strict, pytest, and the regeneration no-op check
```
