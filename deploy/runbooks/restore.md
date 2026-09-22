# Restore

## Trigger

Trigger on PostgreSQL loss/corruption, a failed database volume, or the scheduled weekly restore
drill. The architecture threshold is RPO at most one minute and RTO at most one hour.

## Impact and blast radius

API and processing are unavailable or may reflect an older snapshot. All routes sharing the
database are affected. Deposit addresses remain derivable, but every settlement at or beyond
`cleared` must be reconciled with product GET before resume.

## First 5 minutes

```sh
curl --fail-with-body -sS "$BASE_URL/healthz"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT state,count(*) FROM deposits GROUP BY state ORDER BY state; COMMIT;"
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=200 postgres backup topup
docker compose -f deploy/docker-compose.staging.yml exec -T backup wal-g backup-list
```

Check the restored schema version with the application role, which can read `_sqlx_migrations`
since C7b (#70). Require the newest version shipped by the attested release, `applied` equal to the
number of `*.up.sql` files in `crates/topup/migrations` at the release's source commit, and
`failed=0`:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT version FROM _sqlx_migrations ORDER BY version DESC LIMIT 1; SELECT count(*) AS applied,count(*) FILTER (WHERE NOT success) AS failed FROM _sqlx_migrations; COMMIT;"
```

Never place owner or migration credentials in the service container. **Gap:** `topup
restore-check` is listed by the CLI but exits `restore-check is not implemented` until D3 (PR #58)
lands; use the query above until then.

## Decision tree

- Planned drill and a verified backup exists: proceed only in a throwaway CVM.
- Primary database unavailable: declare incident and restore to a new encrypted volume/CVM.
- Backup list empty, stale, or unverifiable: do not resume; escalate data-loss risk.

## Remediation

**HUMAN-ONLY:** execute D3's reviewed `deploy/RESTORE.md` once #58 merges. This runbook delegates
restore execution to that procedure. Current `main` does not contain the encrypted key fallback,
WAL fetch, throwaway-CVM drill, or implemented restore check, so it intentionally stops rather than
inventing commands or using database-owner credentials in the service container.

With PostgreSQL restored and the service still stopped, run the architecture section 13 restore
gate from C8. It `GET`s the product for every deposit at or beyond `cleared`, adopts the product's
answer, and exits non-zero while any settlement is incomplete. Repeat until it exits `0`; never
resume traffic on a failing gate:

```sh
docker compose -f deploy/docker-compose.staging.yml run --rm topup topup reconcile --once --post-restore --route /etc/topup/routes/phala-cloud-sepolia-pha.yaml
```

Then review the restored state read-only:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT d.id,d.state,s.key,s.status,s.resend_forbidden FROM deposits d LEFT JOIN settlements s ON s.deposit_id=d.id WHERE d.state IN ('cleared','credited','swept') ORDER BY d.updated_at; SELECT count(*) FILTER (WHERE delivered_at IS NULL) AS pending_outbox FROM outbox; COMMIT;"
```

## Verification

Require `topup reconcile --once --post-restore` to exit `0`, no open `post_restore_settlement`
finding, the expected migration version with no failed migration, no duplicate credit, RPO and RTO
evidence, attestation verification, and a human review before traffic resumes.

## Rollback

Keep the restored CVM isolated. Return traffic to the prior healthy CVM only if it is authoritative;
otherwise perform a forward restore from an older verified backup and repeat reconciliation.
