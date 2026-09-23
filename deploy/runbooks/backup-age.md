# Backup age

## Trigger

Trigger on `TopupBackupTooOld` (PR #56: successful backup marker older than 120 seconds), WAL
archiving failures, or `wal-g backup-list` failing to read storage. D3 provides the encrypted base
backups, WAL archiving, and `key-versions/` metadata described in `deploy/RESTORE.md`.

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

- Restore window or staging restore drill in progress (`deploy/RESTORE.md`): expected. The
  replacement runs with `TOPUP_WAL_ARCHIVE=off` and never refreshes the marker, and its `topup` is
  stopped, so its scrape target is also down. Silence `TopupBackupTooOld` and the target-down alert
  for that instance only, and lift them when the restore resumes archiving or the drill CVM is
  destroyed. Never silence them for the live instance.

- Idle-database margin: the only WAL on an idle database is the heartbeat's row every 60 seconds,
  and `archive_timeout=60` switches a segment only once new WAL exists. If a heartbeat commits just
  after a switch check, the next switch waits for the following check, so the marker can reach
  about 120 seconds plus the `wal-push` upload time before it refreshes (usually it refreshes every
  60 seconds). `TopupBackupTooOld` needs the marker above 120 seconds for a full minute, so this
  worst case does not page; a marker that stays past 180 seconds does. The local infra smoke
  bounds the idle marker at 150 seconds.
- Archiver shows no new failures (`failed_count` unchanged, `last_failed_time` empty or older than
  `last_archived_time`), `last_archived_time` is older than two minutes, and the database is idle
  (`SELECT pg_current_wal_lsn()` does not advance across 60 seconds): the `heartbeat` service has
  stopped, so nothing gives `archive_timeout` a segment to switch. Check
  `docker compose -f deploy/docker-compose.staging.yml ps heartbeat` and search the `heartbeat` logs
  for `failed to record restore heartbeat`; fix the reported database connection or credential
  error, then run `docker compose -f deploy/docker-compose.staging.yml restart heartbeat` and confirm
  `last_archived_time` advances within two minutes. Right after a PostgreSQL restart, also search
  the `backup` logs for `startup CHECKPOINT failed`: without that checkpoint PostgreSQL ignores
  `archive_timeout` for up to `checkpoint_timeout`; restart `backup` once the database accepts
  connections.
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
