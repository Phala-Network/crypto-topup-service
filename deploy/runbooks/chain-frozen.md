# Chain frozen

## Trigger

Trigger on `TopupReconciliationMismatch{check="address_derivation"}` (PR #56), a
`reconciliation_blocks` row with `scope='chain'`, product reports of `423 chain_frozen` from address
issuance or rate-lock creation, or `TopupScannerLag` on a chain whose scanner has paused.

## Impact and blast radius

The factory's `addressOf(salt)` disagrees with a stored address, so the service can no longer prove
where customer funds for that chain go. C8 writes `chain:<chain_id>` and, until the block row is
removed: pumps leave the chain's deposits waiting, its scanner pauses, the flusher plans nothing,
and address issuance and rate-lock creation answer `423 chain_frozen`. Other chains keep running.
Credited facts are never rolled back.

## First 5 minutes

Identify the block and the salt/address pairs that disagree:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT block_key,check_name,reason,created_at FROM reconciliation_blocks
WHERE scope='chain' AND chain_id=:chain_id;
SELECT f.subjects->>'address_id' AS address_id,f.subjects->>'salt' AS salt,
       f.expected->>'address' AS factory_address,f.observed->>'address' AS stored_address,
       a.kind,a.version,a.lock_ref,a.account_id,f.created_at
FROM reconciliation_findings f
LEFT JOIN addresses a ON a.id::text=f.subjects->>'address_id'
WHERE f.check_name='address_derivation' AND f.subjects->>'chain_id'=:'chain_id'
ORDER BY f.created_at DESC LIMIT 50;
COMMIT;
SQL
```

Confirm the factory answer through both providers and check the deployed contract tuple against the
attested route:

```sh
cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_B_URL"
cast call "$FACTORY" 'implementation()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$IMPLEMENTATION" 'treasury()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
```

Tell the product that deposits on the chain are temporarily unavailable and follow
[Incident communication](incident-communication.md). Customers must not be shown an address.

## Decision tree

- Providers disagree about `addressOf`: follow [Provider disagreement](provider-disagreement.md)
  before trusting either answer.
- Factory, implementation, or treasury differs from the attested route: treat as a configuration or
  deployment incident; engage Security and Finance and do not lift the block.
- Stored address differs while the contracts match: treat as database corruption or tampering;
  preserve evidence, compare with the latest backup, and escalate to Security.
- Any funds already reached a stored address that the factory does not derive: open a Finance and
  Security incident for those funds.

## Remediation

There is no in-service repair for a derivation mismatch. When the contracts are wrong, correct them
with a new route/config version. When stored rows are wrong, there is no per-row restore; the two
options are:

- a full database restore to a point before the corruption, following [Restore](restore.md); or
- a **HUMAN-ONLY** forward fix of the affected `addresses` rows by the database owner, signed off by
  Security and Finance, with the finding, both providers' `addressOf` answers, and the before and
  after rows recorded as incident evidence.

The application role cannot delete blocks. After Engineering, Security, and Finance sign off, the
database owner lifts the freeze from an owner session outside the service container (**HUMAN-ONLY**;
never place owner credentials in the service container):

```sql
DELETE FROM reconciliation_blocks WHERE block_key = 'chain:<chain_id>';
```

Components resume on their next iteration without a restart. If any stored address still disagrees,
the next reconciliation pass freezes the chain again.

## Verification

Run one pass and require exit `0`, no new `address_derivation` finding, and no `chain:<chain_id>`
row afterward:

```sh
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup reconcile --route /etc/topup/routes/phala-cloud-sepolia-pha.yaml
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT count(*) FROM reconciliation_blocks WHERE scope='chain' AND chain_id=:chain_id; COMMIT;"
```

Then confirm the scanner cursor advances, address issuance answers normally, and the flusher plans
again.

## Rollback

The freeze is the safe state. If issues reappear after lifting it, the reconciler re-freezes the
chain automatically; do not add or remove block rows by hand as a control.
