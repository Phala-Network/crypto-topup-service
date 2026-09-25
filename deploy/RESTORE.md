# Backup and restore runbook

This runbook implements architecture sections 13-15. A restore is a bootstrap from backup, the
pattern of CloudNativePG's `bootstrap.recovery` and Spilo's clone from backup: a new instance of
the same dstack application boots with an empty database volume and the
[restore-time environment](#restore-time-environment), and PostgreSQL itself fetches the newest
base backup, replays every archived WAL segment, and promotes. Production OS images have no SSH or
logs, so no step runs inside the CVM: the operator sets the environment, creates the instance, and
verifies it only through its `/healthz` and read API on its own gateway URL.

## Backup key and metadata

`keys` derives the current `backup/vN` dstack key plus the comma-separated retained versions
in `TOPUP_BACKUP_KEY_FALLBACK_VERSIONS` (and the database credentials, into their own volumes; see
[README](README.md#database-credentials)). It writes backup keys only to the Compose `walg_key`
tmpfs:

```text
/run/wal-g/backup.key       current version used for new uploads
/run/wal-g/backup-v1.key    retained version 1
/run/wal-g/backup-v0.key    retained version 0
```

Every file is atomically published as UID/GID `999:999`, mode `0600`; the directory is mode `0700`.
WAL-G receives the path through `WALG_LIBSODIUM_KEY_PATH`. No key value appears in an image,
environment variable, command line, object metadata, or log.

A replacement CVM of the same app id derives the same database passwords that the restored
cluster's roles already carry, so it needs no database secret; a failed login on the restored
cluster means the app identity is wrong (stop as for a key mismatch).

Every successful upload writes an unencrypted, uncompressed metadata object containing only the
integer key version:

```text
key-versions/base/<backup-name>.json
key-versions/wal/<wal-segment>.json
key-versions/current.json
```

`walg-base-backup` records base-backup metadata and `walg-wal-push` records every WAL segment.
`walg-backup-fetch` selects the base key from its metadata. `walg-restore-command` selects each WAL
key independently, so one recovery range may cross key rotations. Keep all listed `backup/vN`
domains until the corresponding base backups and WAL have expired.

`walg-wal-push` sets `WALG_UPLOAD_CONCURRENCY=1` and `TOTAL_BG_UPLOADED_LIMIT=1` for `wal-push`.
WAL-G otherwise starts a background uploader that archives adjacent `.ready` segments with
`WALG_UPLOAD_CONCURRENCY - 1` workers, up to `TOTAL_BG_UPLOADED_LIMIT - 1` files, and those
objects would have no key-version metadata. With either value at `1` the background uploader is
disabled, so every WAL object is uploaded and annotated by its own `archive_command` call.
`walg-restore-command` likewise sets `WALG_DOWNLOAD_CONCURRENCY=1` so `wal-fetch` never prefetches
an adjacent segment with the wrong key. Base backups keep WAL-G's default concurrency. Do not relax
the wrapper values without replacing the per-object metadata protocol.

`wal-g backup-list` does not decrypt backup data. Never use it as a key test. A key is verified only
by fetching the selected base backup into an empty volume and checking the extracted `PG_VERSION`.

## Weekly local drills

Run both modes weekly:

```sh
make restore-drill
# Or separately:
deploy/local/restore-drill.sh controlled
deploy/local/restore-drill.sh crash
```

`controlled` forces a WAL switch and requires the last source marker and LSN. `crash` waits for a
natural `archive_timeout=60` upload, continues writing, kills PostgreSQL without another switch, and
reports observed loss. The output separates two server-side measurements: `archive_wait_seconds`
runs from the first drill write into the WAL segment until PostgreSQL closes it (its
`archive_status/<segment>.ready` mtime), and `upload_latency_seconds` runs from that close until
the object store's `LastModified` for the uploaded WAL object. Controlled mode also builds a WAL backlog under
key v1 while object storage is down, archives one segment with a single v1 wrapper call and proves
no adjacent segment was uploaded, rotates PostgreSQL to v2 while the rest are pending, lets the
archiver finish them under v2, decrypts every rotation segment with its recorded key version (and
proves the other version fails), and restores across the rotation boundary.

Both modes then destroy the source database and its key volumes and boot the whole stack at once,
as dstack does, with the [restore-time environment](#restore-time-environment). They require that
PostgreSQL restored the newest base backup and promoted with `archive_mode=off`, that `heartbeat`
fails closed and `backup` idles, that the storage credentials are read-only, that `/healthz` serves
a `restore-check` report with `"status":"ok"` and a complete post-restore reconciliation, that a
write request is refused with `503` while a read reaches authentication, that the re-derived
application login works, that the RPO against the externally recorded source heartbeat and LSN is
within bounds, that RTO is at most 3600 seconds, and that the object-storage listing is unchanged
after promotion. They remove their uniquely named Compose projects, volumes, and per-run image
tags.

The `Restore drill` workflow (`.github/workflows/restore-drill.yml`) runs `make restore-drill` every
Monday at 03:17 UTC on the CI runner and can be started manually. Instead of the full drill, the
`deployment` CI job (pull requests and pushes to `main`) runs the bounded `walg-cron`, WAL-G
restore-command, and archive and restore switch tests on the self-hosted runner. The CI runner
reaches the host Docker daemon through its socket and the daemon cannot see the checkout, so the
drill never bind-mounts a host path: `deploy/local/restore-drill.compose.yml` swaps the local
stack's file mounts for project volumes that the drill fills with `docker cp`, locally and on CI
alike.

## Restore-time environment

When a replacement instance is created, dstack's `app-compose.sh` immediately runs
`docker compose up --remove-orphans -d` for the whole attested compose with the encrypted
environment the instance was created with. The replacement therefore always boots, for a real
restore as well as a drill, with a restore-time environment:

- `TOPUP_RESTORE_FROM_BACKUP=on`. The PostgreSQL entrypoint finds the data directory empty,
  selects the newest base backup (`wal-g backup-list`, greatest `time`), fetches it with
  `walg-backup-fetch` beside the data directory, and moves it into place only when complete, with
  `recovery.signal`. PostgreSQL then replays every archived segment through `walg-restore-command`
  and promotes at the end of the archive. A data directory that holds anything is started as is
  and never overwritten; an interrupted fetch starts over and an interrupted recovery resumes. While
  the switch is on, archiving is forced off whatever `TOPUP_WAL_ARCHIVE` says, `walg-cron` refuses
  `wal-push`, and `backup` idles (`base backups are disabled while TOPUP_RESTORE_FROM_BACKUP=on`),
  so neither a base backup nor `wal-g delete retain` touches the prefix. `deploy/tests/walg-archive-switch.sh`
  verifies the switch on the image.
- `TOPUP_WAL_ARCHIVE=off`, which the switch implies as well.
- `TOPUP_SERVICE_ENABLED=read-only`: `topup run` serves only `GET` and `HEAD` requests (anything
  else answers `503`) and runs no scanner, pump, flusher, webhook delivery, reconciler, or
  lease-owner lock; it skips the startup contract check because it issues nothing. `heartbeat`
  exits at its configuration check (`heartbeat is disabled while TOPUP_SERVICE_ENABLED=read-only`).
- `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` for credentials that can only list and read
  `WALG_S3_PREFIX`, so a mistake cannot write, overwrite, or delete backup objects. On Cloudflare
  R2 this is an R2 API token with the permission **Object Read only**, scoped to the backup bucket
  only (staging: `crypto-topup-test`), with `AWS_SESSION_TOKEN` empty.
- `TOPUP_PUBLIC_ORIGIN=https://pending.invalid` at creation; the instance's own gateway URL is
  known only afterwards ([Verify the restored instance](#verify-the-restored-instance)).
- Everything else as for production, including `TOPUP_BACKUP_KEY_VERSION` and
  `TOPUP_BACKUP_KEY_FALLBACK_VERSIONS` (current version first, then every retained version).

After PostgreSQL has promoted (its health check passes only once it is out of recovery; the start
period allows the one-hour RTO) and `migrate` has confirmed the schema, the `restore-check` service
runs once. It verifies migration checksums, WAL state, and table counts, then runs the same
post-restore round as `topup reconcile --post-restore` (architecture section 13): every deposit in
`cleared`, `credited`, or `swept`, plus deposits rejected by the product after reaching `cleared`,
is queried by signed product GET and the product's answer is adopted into the restored database.
It writes its report to the `observability` volume, and the read-only `topup` serves it on
`/healthz`:

```json
{"mode":"read-only","restore_check":{"status":"ok","failures":[],"rpo_basis":"unanchored","restored_heartbeat_at":"…","latest_applied_lsn":"…","row_counts":{…},"post_restore_reconciliation":{"status":"complete","failed_checks":[],"findings":[…]},…}}
```

`restore_check` is `null` until the check has finished. A check that could not run reports
`{"status":"failed","failures":["…"]}`. The boot-time check has no source anchor
(`"rpo_basis":"unanchored"`): the operator compares `restored_heartbeat_at` with their own. A
product lookup that fails, is not found, is still processing, fails identity verification, or
would need an unsafe transition makes `status` `incomplete` and is listed in `failures`. The
regular reconciliation checks run in the same round; their findings and `failed_checks` (for
example an unreachable chain RPC) are reported but do not gate resume. With
`TOPUP_RESTORE_FROM_BACKUP=off`, `restore-check` exits at once on every boot.

Alerts during the restore window are expected, not incidents. The replacement archives nothing
and refreshes no backup-age marker, so `TopupBackupTooOld` fires whenever an old or missing marker
is scraped, and the read-only `topup` exports no metrics, so the monitoring collector's
scrape-target-down alert for the instance fires. Silence both, scoped to the replacement instance,
from creation until [Resume](#resume-real-restore-only) shows a fresh archived segment and the
marker is younger than 120 seconds. During a staging drill, silence them for the drill instance
only, never for the live staging instance, and remove the silences when the drill instance is
deleted.

### The restore env file

`restore.env` holds every production secret plus the restore overrides in plaintext. Create it
only on the operator's machine, in a private directory, and only for this restore. It has one
`KEY=VALUE` line for every name in `deploy/app-compose.example.json` `allowed_envs`, with the
production values (staging values for a drill) except these overrides:

```sh
umask 077
export RESTORE_ENV_DIR="$(mktemp -d)"
cat >"$RESTORE_ENV_DIR/restore.env" <<'EOF'
TOPUP_RESTORE_FROM_BACKUP=on
TOPUP_WAL_ARCHIVE=off
TOPUP_SERVICE_ENABLED=read-only
TOPUP_PUBLIC_ORIGIN=https://pending.invalid
AWS_ACCESS_KEY_ID=<read-only restore key id>
AWS_SECRET_ACCESS_KEY=<read-only restore secret>
AWS_SESSION_TOKEN=
EOF
```

Append every other `allowed_envs` name with its production value, then require that both checks
print nothing (no name duplicated, missing, or extra):

```sh
jq -r '.allowed_envs[]' deploy/app-compose.example.json | sort >"$RESTORE_ENV_DIR/expected"
cut -d= -f1 "$RESTORE_ENV_DIR/restore.env" | sort | uniq -d
cut -d= -f1 "$RESTORE_ENV_DIR/restore.env" | sort -u | diff - "$RESTORE_ENV_DIR/expected"
```

The names must already be the application's `allowed_envs`, which the compose hash fixes: an
instance of a compose revision without `TOPUP_RESTORE_FROM_BACKUP` cannot restore on boot. Delete
the file when the instance exists, and also when the restore is abandoned:

```sh
shred -u "$RESTORE_ENV_DIR/restore.env"
rm -rf "$RESTORE_ENV_DIR"
```

## Create the replacement instance

A replacement derives the same `backup/vN` keys and database passwords only under the original
dstack application identity: the same app id and KMS, with an allowed attested compose. It is a new
instance of that app, never a new app. Pass `--env-file` on every create: without it, Phala Cloud
gives the new instance the encrypted environment of an existing instance of the app, and a
replacement booting with production values would archive a fresh cluster into the production WAL
prefix (colliding segment names and rewriting `key-versions/current.json`), let `backup` push an
empty base backup and run `wal-g delete retain`, and run `topup` with the application's keys.

Each instance gets its own gateway endpoint, `https://<instance_id>-<port>.<gateway.base_domain>`;
Phala Cloud does not distribute traffic across the instances of an app. Still, while a
replacement runs next to a live instance, check that the live URL keeps answering as the live
instance (an empty `200` on `/healthz`, never the read-only JSON).

### Phala Cloud KMS (staging)

With `--kms phala` there is no on-chain authorization: `phala instances add` creates and boots the
instance in one call (CLI 1.1.22, `cli/src/commands/instances/add`). Name the compose revision
explicitly, as the live instance's (`cvms get --json` `.compose_hash`), and record the returned
`vm_uuid`:

```sh
npx --yes phala@1.1.22 cvms get "$SOURCE_CVM_ID" --json >source.json
export APP_ID="$(jq -er '.app_id' source.json)"
npx --yes phala@1.1.22 instances add --app-id "$APP_ID" \
  --compose-hash "$(jq -er '.compose_hash' source.json)" \
  --env-file "$RESTORE_ENV_DIR/restore.env" --name crypto-topup-restore --json >instance.json
export RESTORE_CVM_ID="$(jq -er '.vm_uuid' instance.json)"
```

### On-chain KMS (production)

These are **HUMAN-ONLY** steps using Phala Cloud credentials and the Finance Safe.

1. Retrieve the original incident records and set the original `DstackApp` authorization contract:

   ```sh
   export ORIGINAL_APP_ID=0x<original-dstack-app-address>
   export APP_AUTH_CONTRACT="$ORIGINAL_APP_ID"
   export ORIGINAL_COMPOSE_HASH=0x<last-approved-compose-hash>
   cast call "$APP_AUTH_CONTRACT" 'owner()(address)' --rpc-url "$ETH_RPC_URL"
   ```

   Require the owner to be the recorded Finance Safe and the KMS chain/root to match the failed
   instance. Stop if the original app id or KMS root cannot be established.

2. Prepare a new instance of that existing app with `phala instances add`, not the new-application
   workflow; `phala cvms replicate` needs the source CVM, which may be gone. The commands and JSON
   paths below match phala CLI 1.1.22. Reuse the retained attested compose revision and pass
   [the restore env file](#the-restore-env-file). `--env-file` encrypts it with the key of an
   existing instance record of the app, so stop the failed CVM but do not delete it before this
   step. `NODE_ID` is the numeric node (teepod) id to run the replacement on:

   ```sh
   npx --yes phala@1.1.22 nodes list --json
   export NODE_ID=<node id>
   npx --yes phala@1.1.22 instances add --app-id "$ORIGINAL_APP_ID" --node-id "$NODE_ID" \
     --compose-hash "$ORIGINAL_COMPOSE_HASH" --env-file "$RESTORE_ENV_DIR/restore.env" \
     --prepare-only --json > prepare.json
   ```

   The prepare output uses camelCase keys, and only the `onchainStatus` object is snake_case.
   Validate every field's type before reading it, because `jq -r` prints `null` for a missing path
   and `export X="$(…)"` would hide the failure. Hex values may lack `0x`, so normalize them. Record
   whether the host device was already allowed: another CVM of this app may run on the same host.

   ```sh
   jq -e '(.appId | type == "string") and (.composeHash | type == "string")
     and (.deviceId | type == "string") and (.commitToken | type == "string")
     and (.kmsInfo.chain_id != null)
     and (.onchainStatus.compose_hash_allowed | type == "boolean")
     and (.onchainStatus.device_id_allowed | type == "boolean")' prepare.json
   hex() { printf '0x%s' "${1#0x}" | tr 'A-F' 'a-f'; }
   test "$(hex "$(jq -r '.appId' prepare.json)")" = "$(hex "$ORIGINAL_APP_ID")"
   export COMPOSE_HASH="$(hex "$(jq -r '.composeHash' prepare.json)")"
   test "$COMPOSE_HASH" = "$(hex "$ORIGINAL_COMPOSE_HASH")"
   export DEVICE_ID="$(hex "$(jq -r '.deviceId' prepare.json)")"
   test "$(jq -r '.kmsInfo.chain_id' prepare.json)" = "$(cast chain-id --rpc-url "$ETH_RPC_URL")"
   export COMPOSE_HASH_PREVIOUSLY_ALLOWED="$(jq -r '.onchainStatus.compose_hash_allowed' prepare.json)"
   export DEVICE_PREVIOUSLY_ALLOWED="$(jq -r '.onchainStatus.device_id_allowed' prepare.json)"
   export COMMIT_TOKEN="$(jq -r '.commitToken' prepare.json)"
   jq '{appId, composeHash, deviceId, chain_id: .kmsInfo.chain_id, onchainStatus}' prepare.json
   ```

   Record `DEVICE_PREVIOUSLY_ALLOWED` (`true` or `false`) in the incident log.

3. **Finance Safe:** authorize what is not yet allowed on the original contract: `addComposeHash`
   only when `COMPOSE_HASH_PREVIOUSLY_ALLOWED` is `false`, and `addDevice` only when
   `DEVICE_PREVIOUSLY_ALLOWED` is `false`. Submit the calldata through the Safe, wait for finality,
   then verify both reads return `true`:

   ```sh
   cast calldata 'addComposeHash(bytes32)' "$COMPOSE_HASH"
   cast calldata 'addDevice(bytes32)' "$DEVICE_ID"
   cast call "$APP_AUTH_CONTRACT" 'allowedComposeHashes(bytes32)(bool)' "$COMPOSE_HASH" \
     --rpc-url "$ETH_RPC_URL"
   cast call "$APP_AUTH_CONTRACT" 'allowedDeviceIds(bytes32)(bool)' "$DEVICE_ID" \
     --rpc-url "$ETH_RPC_URL"
   ```

4. Commit the prepared replacement only after both authorizations are final; it boots with the
   restore env at once. Pass the Safe transaction hash, or `already-registered` when nothing was
   added, and the compose hash exactly as the server returned it, as the CLI's own commit path
   does, not the normalized `0x` form used for `cast`:

   ```sh
   npx --yes phala@1.1.22 instances add --app-id "$ORIGINAL_APP_ID" --commit \
     --token "$COMMIT_TOKEN" --compose-hash "$(jq -r '.composeHash' prepare.json)" \
     --transaction-hash "${AUTH_TX_HASH:-already-registered}" --json >instance.json
   export RESTORE_CVM_ID="$(jq -er '.vm_uuid' instance.json)"
   ```

   Fetch `cvm.json` and `attestation.json`, then run the normal compose verification:

   ```sh
   deploy/verify-attested-compose.sh \
     attestation.json cvm.json deploy/docker-compose.yml
   ```

## Verify the restored instance

1. Derive the instance's own URL and wait (at most the one-hour RTO) until `/healthz` serves a
   report:

   ```sh
   npx --yes phala@1.1.22 cvms get "$RESTORE_CVM_ID" --json >restore-cvm.json
   export RESTORE_URL="https://$(jq -er '.instance_id' restore-cvm.json)-8080.$(jq -er '.gateway.base_domain' restore-cvm.json)"
   curl -fsS "$RESTORE_URL/healthz" | tee healthz.json | jq -e '.mode == "read-only" and .restore_check != null'
   ```

2. Require `.restore_check.status == "ok"`, `.restore_check.post_restore_reconciliation.status ==
   "complete"`, the expected `latest_migration`, plausible `row_counts`, and, against the source
   point recorded outside the lost database (the time of the failure, or for a drill the time the
   instance was created), `restored_heartbeat_at` at most 120 seconds older (RPO 60 seconds plus
   one heartbeat interval).

3. Request an application-bound quote with a fresh nonce and verify it as in
   [Attestation, ingress, and egress](README.md#attestation-ingress-and-egress), requiring the
   original app id; a mismatch means this instance does not hold the application identity. Stop.

   ```sh
   export NONCE="$(openssl rand -hex 32)"
   curl -fsS "$RESTORE_URL/v1/attestation?nonce=$NONCE" > restore-attestation.json
   jq -e --arg app "$(printf '%s' "${APP_ID#0x}" | tr 'A-F' 'a-f')" \
     '(.app_id | ascii_downcase | ltrimstr("0x")) == $app' restore-attestation.json
   ```

4. Signed product requests carry the URL they were sent to in `@target-uri`, so set the instance's
   own URL as its origin: replace `TOPUP_PUBLIC_ORIGIN` in a fresh copy of the restore env file
   (same names, so the compose hash is unchanged) and update the instance. It restarts; its data
   directory is no longer empty, so it starts as is, and `restore-check` runs again:

   ```sh
   npx --yes phala@1.1.22 envs update "$RESTORE_CVM_ID" -e "$RESTORE_ENV_DIR/restore.env"
   ```

   Wait for `/healthz` to serve an `ok` report again, then check known deposits with
   product-signed support lookups, for example with the Python SDK and the product's key:
   `TopupClient("$RESTORE_URL", "<product slug>", signer).lookup_deposits(tx_hash=...)` must
   return each deposit in its recorded state.

## Resume (real restore only)

Compare incident markers and expected row counts first. Switching the encrypted environment to
production values restarts the app compose on the restored database, which is then started as
is. Control ingress before the switch: confirm the failed instance is deleted so only one instance
holds the application keys, and have the product owner hold calls to the service
([incident communication](runbooks/incident-communication.md)). Then update the environment with
`TOPUP_RESTORE_FROM_BACKUP=off`, `TOPUP_WAL_ARCHIVE=on`, the read-write object-storage
credentials, `TOPUP_SERVICE_ENABLED=on`, and the production `TOPUP_PUBLIC_ORIGIN`
(`phala envs update "$RESTORE_CVM_ID" -e <production env file>`), and require:

- `/healthz` answers `200` with an empty body (the full service, not the read-only one);
- `backup` finds no base backup on the promoted timeline and takes one at once (`walg-timeline-backup`);
  until it completes the new timeline cannot be restored, so nothing resumes before a
  `base_<timeline>…` backup newer than the switch is listed and new WAL segments of that timeline
  appear with their `key-versions/wal/<segment>.json` objects (list them with the owner's own
  storage credentials);
- a healthy `topup` attestation, and `topup_backup_last_success_unixtime_seconds` scraped from the
  new instance and less than 120 seconds old. Only then remove the restore-window alert silences.

Let the product resume calls only after health and reconciliation remain clean. Addresses need no
separate restore because their salts are deterministic from product data.

## Staging restore drill

The staging drill restores the staging app's real backups into a throwaway instance of the staging
app (a staging-copy app id has a different identity and cannot derive the staging backup keys) and
never leaves [Verify the restored instance](#verify-the-restored-instance). The drill instance must
never write to the source WAL prefix: after promotion it is on a new timeline, and its `.history`
file, segments, and `key-versions/current.json` would make a later real restore follow
`recovery_target_timeline=latest` onto the drill's timeline. It must also never run `backup`
(`wal-g delete retain` on the shared prefix) or the full `topup` (the live application's keys, next
to the live instance). The restore-time environment guarantees all three.

1. Issue an R2 API token with **Object Read only** on the staging bucket only, and build the
   [restore env file](#the-restore-env-file) with it and the live staging values.
2. Silence `TopupBackupTooOld` and the scrape-target-down alert for the drill instance only.
3. Record the drill start time (the RPO anchor), then
   [create the instance](#phala-cloud-kms-staging) and delete the env file.
4. Run [Verify the restored instance](#verify-the-restored-instance) and record the report, RPO,
   and RTO in the drill log. Never switch the drill's environment to production values.
5. Delete the drill instance by its own `vm_uuid` (never by app id or name, which also match the
   live instance), revoke the read-only token, and remove the drill silences:

   ```sh
   npx --yes phala@1.1.22 cvms delete "$RESTORE_CVM_ID" --force
   ```

`deploy/local/restore-drill.sh` mirrors this against local object storage.

## Failure handling

- **App id, KMS, compose, or attestation mismatch:** stop. Correct the original app authorization;
  never copy key files between CVMs.
- **`/healthz` never answers, or `restore_check` stays `null` past the RTO:** the base backup
  could not be listed, fetched, or decrypted, or recovery failed (the WAL wrapper returns `126` on
  decryption, metadata, and storage errors, so PostgreSQL aborts instead of promoting). Delete the
  instance, verify object-storage access, integrity, and the retained version list with the
  owner's credentials, and retry with a new instance. `backup-list` success is not evidence of a
  correct key.
- **WAL metadata spans versions:** keep every referenced version in
  `TOPUP_BACKUP_KEY_FALLBACK_VERSIONS`. The restore wrapper selects each segment's version; never
  rewrite old metadata to the current version.
- **`restore_check.status` is `failed` or `incomplete`:** do not resume. Product state wins, but an
  unsafe reverse transition or failed lookup requires an incident repair and another complete
  check.
- **RTO over 3600 seconds:** escalate even if the eventual restore passes.
