# Backup and restore runbook

This runbook implements architecture sections 13-15. Run it in a new throwaway CVM first. Do not
reuse or attach the production PostgreSQL volume, and do not resume service traffic until
`restore-check` and the product reconciliation step both pass.

## Backup key mechanism

The `backup-key` sidecar runs as UID/GID `999:999`, calls dstack `get_key` with the
versioned secp256k1 domain `backup/v1`, hex-encodes the 32-byte secret, and atomically writes
`/run/wal-g/backup.key` with mode `0600`. The directory is a Compose-managed tmpfs volume with
mode `0700`; the sidecar remains alive to keep that tmpfs mounted, while PostgreSQL and the backup
container mount it read-only. Its healthcheck validates only file size and permissions. WAL-G
receives only:

```text
WALG_LIBSODIUM_KEY_PATH=/run/wal-g/backup.key
WALG_LIBSODIUM_KEY_TRANSFORM=hex
```

The secret value is never stored in the image, Compose environment, command line, or logs. Local
Compose derives the same domain from the dstack simulator. A developer binary built with
`--features dev-signer` may use `topup backup-key --dev` for isolated tests only.

## Weekly local drill

Run the full base-backup plus archived-WAL restore against pinned MinIO images:

```sh
make restore-drill
```

The command destroys only its uniquely named temporary Compose project, prints the JSON
`restore-check` result, the restored marker IDs, measured RPO, and elapsed RTO, then removes its
containers and volumes. Run it weekly because the complete image build and point-in-time recovery
are intentionally not part of the bounded deployment CI job.

## Restore in a throwaway CVM

1. Copy the exact attested Compose and route/chain configuration used by the failed CVM. Pin the
   same `TOPUP_IMAGE` and `POSTGRES_WALG_IMAGE` digests. Configure the object-store variables and
   leave application ingress disabled.
2. Set `TOPUP_BACKUP_KEY_VERSION=1`. Start only the dstack socket integration and the key
   service. Verify metadata, never contents:

   ```sh
   docker compose up -d backup-key
   docker compose ps backup-key
   docker compose run --rm --no-deps --entrypoint /usr/bin/stat postgres \
     -c '%a %u %g' /run/wal-g/backup.key
   # Expected: 600 999 999
   ```

3. Create a fresh empty PostgreSQL volume. Fetch the newest base backup as UID 999:

   ```sh
   docker compose run --rm --no-deps restore '
     test -z "$(ls -A "$PGDATA")"
     wal-g backup-fetch "$PGDATA" LATEST
   '
   ```

4. Choose a recovery target from the incident timeline. To recover all available WAL, omit
   `recovery_target_lsn`; otherwise set the last verified archived LSN. Add the recovery settings
   to the restored data directory and create `recovery.signal`:

   ```text
   restore_command = 'wal-g wal-fetch %f %p'
   recovery_target_lsn = '0/00000000'
   recovery_target_inclusive = true
   recovery_target_action = 'promote'
   ```

5. Start PostgreSQL with application, heartbeat, backup, and public ingress still stopped. Wait
   for `pg_isready` and require `SELECT NOT pg_is_in_recovery()` to return `true`.
6. Run the owner-credential check without putting the URL on the command line:

   ```sh
   RESTORE_DATABASE_URL='postgres://...' topup restore-check
   ```

   It exits non-zero unless the restored schema matches the newest embedded migration, recovery
   has promoted, a latest applied WAL LSN exists, the newest heartbeat is no older than its
   recorded 60-second RPO, and durable table counts are readable and sane.
7. Before resuming, reconcile every deposit in `cleared`, `credited`, or `swept` and every
   settlement not in `accepted`/`rejected` by calling the product `GET` endpoint with the stored
   idempotency key. The product answer is authoritative. C6/C8 are not on the current main branch,
   so this D3 command reports `hook_pending_c6_c8` and the exact pending counts; an operator must
   not treat that marker as completed reconciliation.
8. Compare expected row counts and incident markers, enable the backup and heartbeat services,
   confirm a new WAL archive reaches object storage, then enable the application and ingress.
   Addresses require no separate restore because their salts are deterministic from product data.

## Prior backup key versions

Keep every prior dstack backup domain. WAL-G decrypts one libsodium key at a time, so try versions
without changing the backup objects:

```sh
for version in 1 0; do
  TOPUP_BACKUP_KEY_VERSION=$version docker compose up -d --force-recreate backup-key
  if docker compose run --rm --no-deps backup wal-g backup-list; then
    echo "backup key version $version selected"
    break
  fi
done
```

Use the selected version for both `backup-fetch` and every `wal-fetch`. Never overwrite or delete
old dstack domains while retained backups depend on them. After restore, switch back to the current
version before taking a new base backup.

## Failure handling

- **Key/decryption failure:** stop. Try the documented prior version list. Do not create a new key
  under an old version number and do not alter backup objects.
- **Missing WAL or target not reached:** keep the failed volume for inspection, run `wal-g wal-show`
  and `wal-g wal-verify integrity`, choose only a verified earlier target, and record the resulting
  RPO breach before retrying with another fresh volume.
- **Schema, heartbeat, WAL, or row-count failure:** do not resume. Preserve command output and
  PostgreSQL/WAL-G logs, open an incident, and restore again from an earlier intact base backup.
- **Product reconciliation mismatch:** product state wins. Apply only the safe repair defined in
  architecture section 13; alert and keep settlement/flush processing paused for every other case.
- **RTO over one hour:** escalate the incident even if the eventual restore passes.
