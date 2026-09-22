# Refund execution

## Trigger

Trigger when Finance has approved a refundable wrong-asset, overpayment, rejected-not-sanctioned,
or late closed-workspace deposit. Credited funds and dust below policy are not refundable.

## Impact and blast radius

One treasury transfer is at risk. A wrong destination or amount is irreversible; never default to
the deposit sender because it may be an exchange hot wallet.

## First 5 minutes

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=refund_id="$REFUND_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT r.id,r.deposit_id,r.amount_atomic::text,r.to_address,r.status,r.tx_hash,
       d.state,d.reason,d.asset_contract,d.amount_atomic::text AS deposit_amount
FROM refunds r JOIN deposits d ON d.id=r.deposit_id WHERE r.id=:'refund_id'::uuid;
COMMIT;
SQL
cast call "$TOKEN" 'balanceOf(address)(uint256)' "$TREASURY" --rpc-url "$RPC_PROVIDER_A_URL"
```

**HUMAN-ONLY:** independently verify the customer's destination-control evidence and Finance policy
approval.

## Decision tree

- Sanctioned, credited, dust below threshold, or destination unverified: stop and reject/escalate.
- Approved ERC-20 refund: execute token transfer net of approved gas policy.
- Native/wrong asset unsupported by this route: require a separately reviewed Safe transaction.

## Remediation

The approve endpoint is present but returns C12 HTTP `501` on `main`; record the gap and do not
write the status directly. Once C12 is merged, sign the request and require HTTP 200:

```sh
: > /tmp/empty
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/refunds/$REFUND_ID/approve" /tmp/empty "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl -sS -X POST -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/empty "$BASE_URL/v1/admin/refunds/$REFUND_ID/approve"
cast calldata 'transfer(address,uint256)' "$REFUND_TO" "$REFUND_AMOUNT_ATOMIC"
```

**HUMAN-ONLY, Finance Safe:** submit the calldata to `$TOKEN`, wait for finality, and capture the
transaction hash. The record endpoint also returns `501` on `main`:

```sh
printf '%s' "{\"tx_hash\":\"$REFUND_TX_HASH\"}" > /tmp/refund-record.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/refunds/$REFUND_ID/record" /tmp/refund-record.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/refund-record.json "$BASE_URL/v1/admin/refunds/$REFUND_ID/record"
```

## Verification

Receipt status is successful/finalized, token amount and destination match, refund becomes
confirmed, `deposit.refunded` is delivered once, and reconciliation is clean.

## Rollback

No on-chain rollback exists. If the service record is wrong but transfer is correct, keep evidence
and apply only a reviewed forward repair after C12; never alter append-only audit/transitions.
