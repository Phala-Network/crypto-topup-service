# Refund execution

**Trigger:** the product filed a refund request (`POST /v1/refunds`; stored status `requested`,
the product sees `pending`) and Finance approved it under architecture §15: a wrong token, below the
minimum credit but at or above `min_refund_atomic`, rejected for any reason but sanctions, or
funds that arrived after the workspace closed; or a credited deposit whose credit the product
did not apply or has reversed (confirm with the product before approving). Sanctioned funds and
dust below policy are not refundable.

**Impact:** one irreversible treasury transfer. Never default the destination to the deposit's
sender, which may be an exchange hot wallet.

## First steps

1. Read the deposit with a support lookup (state, reason, amount, token) and the treasury balance:
   `cast call "$TOKEN" 'balanceOf(address)(uint256)' "$TREASURY" --rpc-url "$RPC_PROVIDER_A_URL"`.
2. **HUMAN-ONLY:** verify the customer's control of the destination and Finance's approval.

## Decide

- Sanctioned, credited, dust, or an unverified destination: stop.
- Route token: a token transfer from the treasury, net of the approved gas policy.
- Unsupported token or native coin: a separately reviewed Safe transaction
  ([rejected funds at treasury](rejected-funds-at-treasury.md)).

## Fix

Approve (the service re-checks the deposit, route, product, and account; require `200` with
`status` `approved`), then prepare the transfer:

```sh
admin POST "/v1/admin/refunds/$REFUND_ID/approve"
cast calldata 'transfer(address,uint256)' "$REFUND_TO" "$REFUND_AMOUNT_ATOMIC"
```

**HUMAN-ONLY, Finance Safe:** submit the calldata to `$TOKEN` from the treasury Safe and wait for
finality. Record the exact hash, which moves the refund to `sent`; the service confirms it at
finality:

```sh
admin POST "/v1/admin/refunds/$REFUND_ID/record" "{\"tx_hash\":\"$REFUND_TX_HASH\"}"
```

Until confirmation, `record` can correct a wrong hash (it is audited); a confirmed hash cannot
change.

## Done when

The refund is `confirmed` (the daily report's `refunds_by_status`), one `deposit.refunded` webhook
reaches the product, and the transfer's amount and destination match.
