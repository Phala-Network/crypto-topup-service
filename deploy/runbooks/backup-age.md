# Backup age

## Trigger

Trigger on `TopupBackupTooOld` (PR #56: successful backup marker older than 120 seconds), WAL
archiving failures, or `wal-g backup-list` failing to read storage. The marker and encrypted
backups come from D3 (PR #58), which is not on `main`.

## Impact and blast radius

The service may continue, but recoverable RPO grows beyond one minute. A database failure can become
a data-loss incident across all routes.

## First 5 minutes

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT archived_count,failed_count,last_archived_wal,last_archived_time,last_failed_wal,last_failed_time FROM pg_stat_archiver; COMMIT;"
docker compose -f deploy/docker-compose.staging.yml exec -T backup wal-g backup-list
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=300 postgres backup
```

## Decision tree

- WAL archiver failing but object storage reachable: fix credentials/permissions and verify a new WAL.
- Object storage unavailable: escalate provider outage; do not delete local WAL.
- Backup age unknown because D3 marker is absent: treat as failed closed.

## Remediation

**HUMAN-ONLY:** correct encrypted environment/object-storage policy through a new attested compose
deployment. `main` lacks D3 encryption, success marker, key fallback, and restore drill. Do not run
an unencrypted manual backup as a substitute.

## Verification

Require fresh archived WAL, a successful encrypted base backup, readable backup list, and a
throwaway restore passing implemented `topup restore-check` within RPO/RTO.

## Rollback

Restore the prior known-good object-storage configuration through the D2 upgrade flow. Never delete
WAL or backup objects during incident response.
