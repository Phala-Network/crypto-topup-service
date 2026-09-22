# crypto-topup-sdk

Python SDK for the crypto top-up service product API (Python 3.12+).

- `topup_client`: generated from `crates/topup/openapi.json`; do not edit.
- `topup_sdk`: request signing, settlement-request and webhook verification, address and
  deposit-id recomputation, and `TopupClient`, whose helpers are idempotent and retry safely.

```python
from topup_sdk import RequestSigner, TopupClient

signer = RequestSigner.from_seed_file("acme/v1", "product.seed")
with TopupClient("https://topup.example", "acme", signer) as client:
    client.register_account("workspace-42")
    lock = client.create_rate_lock("workspace-42", "checkout-1", amount_minor=2500)
    print(lock.address, lock.amount_atomic, lock.expires_at)
```

Create a product key with `uv run topup-sdk keygen --keyid acme/v1 --seed-out product.seed` and
send only the printed public key to the operator. See `docs/sdk.md` for the signing profile and
the versioning and deprecation policy, `sdk/examples/phala_cloud_integration.py` for a complete
integration including the settlement endpoint, and `deploy/sandbox/README.md` for the sandbox.

```sh
make sync    # install the locked environment
make check   # ruff, mypy --strict, pytest, and the regeneration no-op check
```
