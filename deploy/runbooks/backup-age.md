# Backup age

## Trigger

Trigger on `TopupBackupTooOld` (PR #56: successful backup marker older than 120 seconds), WAL
archiving failures, or `wal-g backup-list` failing to read storage. D3 provides the encrypted base
backups, WAL archiving, and `key-versions/` metadata described in `deploy/RESTORE.md`; the backup-age
alert itself is pending #56.

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

- Archiver shows no new failures (`failed_count` unchanged, `last_failed_time` empty or older than
  `last_archived_time`), `last_archived_time` is older than two minutes, and the database is idle
  (`SELECT pg_current_wal_lsn()` does not advance across 60 seconds): the `backup` service or its
  WAL keepalive has stopped, so nothing gives `archive_timeout` a segment to switch. Check
  `docker compose -f deploy/docker-compose.staging.yml ps backup` and search the `backup` logs for
  `WAL keepalive transaction failed`; fix the reported database connection or credential error,
  then run `docker compose -f deploy/docker-compose.staging.yml restart backup` and confirm
  `last_archived_time` advances within two minutes.
- WAL archiver failing but object storage reachable: fix credentials/permissions and verify a new WAL.
- Object storage unavailable: escalate provider outage; do not delete local WAL.
- Backup age unknown (no recent `last_archived_time` or `key-versions/wal/` object): treat as failed
  closed.

## Remediation

**HUMAN-ONLY:** correct encrypted environment/object-storage policy through a new attested compose
deployment. Follow `deploy/RESTORE.md` for key versions and fallbacks. Do not run an unencrypted
manual backup as a substitute.

## Verification

Require fresh archived WAL, a successful encrypted base backup, readable backup list, and a
throwaway restore passing implemented `topup restore-check` within RPO/RTO.

## Rollback

Restore the prior known-good object-storage configuration through the D2 upgrade flow. Never delete
WAL or backup objects during incident response.
