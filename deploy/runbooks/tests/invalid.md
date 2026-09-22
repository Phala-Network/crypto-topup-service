# Invalid runbook fixture

This file must fail `deploy/runbooks/check.sh`; it is not an operator runbook.

```sh
topup bogus
topup attest --bogus
curl -sS -X POST "$BASE_URL/v1/admin/not-a-route"
```
