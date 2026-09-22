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
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "BEGIN TRANSACTION READ ONLY; SELECT max(version) AS latest_migration FROM _sqlx_migrations WHERE success; SELECT state,count(*) FROM deposits GROUP BY state ORDER BY state; COMMIT;"
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=200 postgres backup topup
docker compose -f deploy/docker-compose.staging.yml exec -T backup wal-g backup-list
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup restore-check
```

The last command currently exits `restore-check is not implemented`; D3 is a blocking command gap.

## Decision tree

- Planned drill and a verified backup exists: proceed only in a throwaway CVM.
- Primary database unavailable: declare incident and restore to a new encrypted volume/CVM.
- Backup list empty, stale, or unverifiable: do not resume; escalate data-loss risk.

## Remediation

**HUMAN-ONLY:** execute the reviewed D3 restore procedure once merged. `main` does not contain the
encrypted key fallback, WAL fetch, throwaway-CVM drill, or implemented restore check, so this
runbook intentionally stops rather than inventing commands.

After restore, run read-only reconciliation:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "BEGIN TRANSACTION READ ONLY; SELECT d.id,d.state,s.key,s.status,s.resend_forbidden FROM deposits d LEFT JOIN settlements s ON s.deposit_id=d.id WHERE d.state IN ('cleared','credited','swept') ORDER BY d.updated_at; SELECT count(*) FILTER (WHERE delivered_at IS NULL) AS pending_outbox FROM outbox; COMMIT;"
```

## Verification

Require an implemented `topup restore-check`, clean GET-first adoption, no duplicate credit, RPO
and RTO evidence, attestation verification, and a human review before traffic resumes.

## Rollback

Keep the restored CVM isolated. Return traffic to the prior healthy CVM only if it is authoritative;
otherwise perform a forward restore from an older verified backup and repeat reconciliation.
