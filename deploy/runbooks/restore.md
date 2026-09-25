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

Check the restored schema version with the application role, which can read `_sqlx_migrations`.
Require the newest version shipped by the attested release, `applied` equal to the number of
`*.up.sql` files in `crates/topup/migrations` at the release's source commit (one,
`20260922000000_initial_schema`, until a migration is added), and `failed=0`:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT version FROM _sqlx_migrations ORDER BY version DESC LIMIT 1; SELECT count(*) AS applied,count(*) FILTER (WHERE NOT success) AS failed FROM _sqlx_migrations; COMMIT;"
```

Never place owner or migration credentials in the service container. `topup restore-check` runs in
its dedicated tools service with owner credentials and checks the same migration state in full.

## Decision tree

- Planned drill and a verified backup exists: proceed only in a throwaway instance of the app,
  following the "Staging restore drill" section of `deploy/RESTORE.md` (the restore-check variant
  of the compose, `deploy/render-compose.sh --restore-check`: restore required, archiving and
  base backups off, `topup` read-only; read-only object-storage credentials; verify through
  `/healthz` and signed reads only, then delete the instance).
- Primary database unavailable: declare incident and restore to a new encrypted volume/CVM.
- Backup list empty, stale, or unverifiable: do not resume; escalate data-loss risk.

## Remediation

**HUMAN-ONLY:** execute `deploy/RESTORE.md`: create a new instance of the original app id with the
restore-check variant of the compose. It derives the retained backup keys, restores the newest base backup,
replays encrypted WAL, promotes, and runs the restore check at boot; its report is on the
instance's `/healthz`. This runbook delegates restore execution to that procedure.

On a stack with a shell (a local or sandbox stack), the restore check can also run by hand with an
external anchor, with PostgreSQL restored and `topup`, `heartbeat`, and `backup` stopped. It verifies migrations, WAL position, externally anchored RPO, and table counts, then runs
the architecture section 13 restore gate from C8: it `GET`s the product for every deposit at or
beyond `cleared`, adopts the product's answer, and exits non-zero while any settlement is
incomplete. The gate refuses to start with `lease_owner_lock_held` while `topup run` or
`topup reconcile` is connected to this database; stop that process, then retry. The lock
only sees processes connected to this PostgreSQL, so stopping the old instance remains the
control. A `topup run` started while the gate runs waits for it, retrying with backoff and
logging `waiting for the lease-owner lock`, instead of exiting:

```sh
docker compose -f deploy/docker-compose.staging.yml run --rm --no-deps restore-check --expected-heartbeat-at "$EXPECTED_HEARTBEAT_AT" --expected-lsn "$EXPECTED_LSN"
```

After an incident repair, still with the service stopped, rerun the same restore check with the
same expected values until it reports `"status":"ok"`; never resume traffic on a failing gate.

Then review the restored state read-only:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT d.id,d.state,s.key,s.status,s.resend_forbidden FROM deposits d LEFT JOIN settlements s ON s.deposit_id=d.id WHERE d.state IN ('cleared','credited','swept') ORDER BY d.updated_at; SELECT count(*) FILTER (WHERE delivered_at IS NULL) AS pending_outbox FROM outbox; COMMIT;"
```

## Verification

Require `topup restore-check` to report `"status":"ok"`, no open `post_restore_settlement` finding, the expected migration version with no failed migration, no duplicate credit, RPO and RTO
evidence, attestation verification, and a human review before traffic resumes.

## Rollback

Keep the restored CVM isolated. Return traffic to the prior healthy CVM only if it is authoritative;
otherwise perform a forward restore from an older verified backup and repeat reconciliation.
