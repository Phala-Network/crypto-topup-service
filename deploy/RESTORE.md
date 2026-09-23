# Backup and restore runbook

This runbook implements architecture sections 13-15. Restore into a new throwaway CVM. Keep
application ingress, `topup`, `heartbeat`, and `backup` stopped until every check passes.

## Backup key and metadata

`backup-key` derives the current `backup/vN` dstack key plus the comma-separated retained versions
in `TOPUP_BACKUP_KEY_FALLBACK_VERSIONS`. It writes only to the Compose `walg_key` tmpfs:

```text
/run/wal-g/backup.key       current version used for new uploads
/run/wal-g/backup-v1.key    retained version 1
/run/wal-g/backup-v0.key    retained version 0
```

Every file is atomically published as UID/GID `999:999`, mode `0600`; the directory is mode `0700`.
WAL-G receives the path through `WALG_LIBSODIUM_KEY_PATH`. No key value appears in an image,
environment variable, command line, object metadata, or log.

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
MinIO's `LastModified` for the uploaded WAL object. Controlled mode also builds a WAL backlog under
key v1 while object storage is down, archives one segment with a single v1 wrapper call and proves
no adjacent segment was uploaded, rotates PostgreSQL to v2 while the rest are pending, lets the
archiver finish them under v2, decrypts every rotation segment with its recorded key version (and
proves the other version fails), and restores across the rotation boundary. Both modes run the step
3 selection commands verbatim, start the restored instance with `TOPUP_WAL_ARCHIVE=off` and require
the object-storage listing to be unchanged after promotion, pass the externally recorded source
heartbeat and LSN to `restore-check`, assert RTO is at most 3600 seconds, exercise a signed product
GET, and remove their uniquely named Compose projects, volumes, and per-run image tags.

The `Restore drill` workflow (`.github/workflows/restore-drill.yml`) runs `make restore-drill` every
Monday at 03:17 UTC on the CI runner and can be started manually. Instead of the full drill, the
`deployment` CI job (pull requests and pushes to `main`) runs the bounded `walg-cron`, WAL-G
restore-command, and archive-switch tests on the self-hosted runner. The CI runner reaches the host
Docker daemon through its socket and the daemon cannot see the checkout, so the drill never
bind-mounts a host path: `deploy/local/restore-drill.compose.yml` swaps the local stack's file
mounts for project volumes that the drill fills with `docker cp`, locally and on CI alike.

## Replacement CVM boot environment

When the replacement CVM is committed, dstack's `app-compose.sh` immediately runs
`docker compose up --remove-orphans -d` for the whole attested compose, with the encrypted
environment exported from `/dstack/.host-shared/.decrypted-env`. Every service starts before an
operator can act: PostgreSQL runs `initdb` into an empty volume, `migrate` migrates it, and
`topup`, `heartbeat`, and `backup` start. With production settings that fresh timeline-1 cluster
would archive into the production WAL prefix (colliding segment names and rewriting
`key-versions/current.json`), `backup` could push an empty base backup and run `wal-g delete
retain`, and `topup` would run with the application's keys. The replacement therefore always boots,
for a real restore as well as a drill, with this restore-time encrypted environment:

- `TOPUP_WAL_ARCHIVE=off`: the PostgreSQL entrypoint appends `-c archive_mode=off` after every
  other flag, so no command-line flag can re-enable archiving;
  `deploy/tests/walg-archive-switch.sh` verifies this on the image.
- `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` for credentials that can only list and read
  `WALG_S3_PREFIX`, so a mistake cannot write, overwrite, or delete backup objects.
- `DATABASE_URL` empty: `topup run` and `topup heartbeat` exit at their configuration check
  (`DATABASE_URL is required for …`) before touching the database, keys, or network.
- Everything else as for production, including `MIGRATE_DATABASE_URL`, `TOPUP_BACKUP_KEY_VERSION`,
  and `TOPUP_BACKUP_KEY_FALLBACK_VERSIONS` (current version first, then every retained version).

Alerts during the restore window are expected, not incidents. The replacement archives nothing
and refreshes no backup-age marker, so `TopupBackupTooOld` fires whenever an old or missing marker
is scraped. `topup` is stopped or failing closed and exports no metrics, so the monitoring
collector's scrape-target-down alert for the instance fires. Silence both, scoped to the
replacement instance, from commit until [step 7](#restore-the-database) shows a fresh archived
segment and the marker is younger than 120 seconds. During a staging drill, silence them for the
drill instance only, never for the live staging instance, and remove the silences when the drill
CVM is destroyed.

Run every command below inside the replacement CVM (`npx --yes phala@1.1.22 ssh "$CVM_ID"`)
through dstack's own compose file, project, and decrypted environment. A separate
`docker compose` invocation from another directory would resolve a different project and different
volumes:

```sh
dc() {
  (
    cd /dstack
    eval "$(jq -r 'to_entries[] | "export \(.key)=\(.value | @sh)"' \
      /dstack/.host-shared/.decrypted-env.json)"
    docker compose -f /dstack/docker-compose.yaml "$@"
  )
}
test "$(dc config --format json | jq -r '.name')" = \
  "$(jq -r '.project' /run/dstack/app-compose-runtime.json)"
```

## Authorize the replacement CVM

A fresh CVM derives the same `backup/vN` key only when it runs under the original dstack application
identity: the same app id and KMS root, with an allowed attested compose. These are **HUMAN-ONLY**
steps using Phala Cloud credentials and the Finance Safe.

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
   paths below match phala CLI 1.1.22. Reuse the retained attested compose revision and pass the
   [restore-time environment](#replacement-cvm-boot-environment) as an env file. `--env-file`
   encrypts it with the key of an existing instance record of the app, so stop the failed CVM but
   do not delete it before this step.

   `restore.env` holds every production secret plus the restore overrides in plaintext. Create it
   only on the operator's machine, in a private directory, and only for this restore. It has one
   `KEY=VALUE` line for every name in `deploy/app-compose.example.json` `allowed_envs`, with the
   production values (staging values for a drill) except these overrides:

   ```sh
   umask 077
   export RESTORE_ENV_DIR="$(mktemp -d)"
   cat >"$RESTORE_ENV_DIR/restore.env" <<'EOF'
   TOPUP_WAL_ARCHIVE=off
   AWS_ACCESS_KEY_ID=<read-only restore key id>
   AWS_SECRET_ACCESS_KEY=<read-only restore secret>
   AWS_SESSION_TOKEN=
   DATABASE_URL=
   EOF
   ```

   Append every other `allowed_envs` name with its production value, then require that both checks
   print nothing (no name duplicated, missing, or extra):

   ```sh
   jq -r '.allowed_envs[]' deploy/app-compose.example.json | sort >"$RESTORE_ENV_DIR/expected"
   cut -d= -f1 "$RESTORE_ENV_DIR/restore.env" | sort | uniq -d
   cut -d= -f1 "$RESTORE_ENV_DIR/restore.env" | sort -u | diff - "$RESTORE_ENV_DIR/expected"
   ```

   `NODE_ID` is the numeric node (teepod) id to run the replacement on, taken from the node list:

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
   whether the host device was already allowed: another CVM of this app, possibly the live one, may
   run on the same host.

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

   Record `DEVICE_PREVIOUSLY_ALLOWED` (`true` or `false`) in the incident or drill log; the device
   cleanup below depends on it.

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

4. Commit the prepared replacement only after both authorizations are final. It boots the whole
   compose at once, so the restore-time environment must already be in the prepared encrypted
   environment. Pass the Safe transaction hash, or `already-registered` when nothing was added, and
   the compose hash exactly as the server returned it, as the CLI's own commit path does, not the
   normalized `0x` form used for `cast`. Then destroy the plaintext environment file:

   ```sh
   npx --yes phala@1.1.22 instances add --app-id "$ORIGINAL_APP_ID" --commit \
     --token "$COMMIT_TOKEN" --compose-hash "$(jq -r '.composeHash' prepare.json)" \
     --transaction-hash "${AUTH_TX_HASH:-already-registered}" --json
   shred -u "$RESTORE_ENV_DIR/restore.env"
   rm -rf "$RESTORE_ENV_DIR"
   ```

   If the restore is abandoned before this step, run the same `shred -u` and `rm -rf` anyway.

   Fetch `cvm.json` and `attestation.json`, then run the normal compose verification:

   ```sh
   deploy/verify-attested-compose.sh \
     attestation.json cvm.json deploy/docker-compose.yml
   ```

5. Request a fresh application-bound quote inside the replacement CVM. Verify the quote, TCB, RTMR
   event log, KMS chain, and compose through the official dstack verification flow. The CLI includes
   dstack's reported app id; compare it to the original before using any backup key:

   ```sh
   export NONCE="$(openssl rand -hex 32)"
   dc run --rm --no-deps topup topup attest --nonce "$NONCE" > restore-attestation.json
   jq -e --arg app "$(printf '%s' "${ORIGINAL_APP_ID#0x}" | tr 'A-F' 'a-f')" \
     '(.app_id | ascii_downcase | ltrimstr("0x")) == $app and (.quote | length > 0)' \
     restore-attestation.json
   ```

   A mismatched or unverifiable app id means this instance cannot be trusted to derive the original
   backup key. Stop; do not try decryption and do not create a new key under an old version number.

## Restore the database

1. Record the source failure point outside the destroyed PostgreSQL volume. The `heartbeat` service
   logs `restore heartbeat recorded` once per minute with `recorded_at` (RFC 3339, UTC) and `wal_lsn`
   (the source WAL location read after that heartbeat committed). Copy both values verbatim from the
   last such line in the retained service logs:

   ```sh
   export EXPECTED_HEARTBEAT_AT=2026-09-22T14:35:18.172465Z
   export EXPECTED_LSN=0/5000060
   ```

   Without a heartbeat timestamp the RPO cannot be proven: continue only as an explicitly declared
   incident exception and keep service traffic stopped. If only the LSN is missing, omit
   `--expected-lsn` in step 6 and record the exception in the incident; the report then shows
   `"rpo_basis":"heartbeat_only"` and `"wal_bytes_behind":null`, so the RPO rests on the heartbeat
   timestamp alone.

2. Confirm the boot environment took effect, then stop everything except `backup-key` and replace
   the `initdb` volume with an empty one. Check the key file's metadata, never its contents:

   ```sh
   dc exec -T postgres psql -U postgres -d topup -Atc 'SHOW archive_mode'
   # Expected: off. If it is on, stop postgres immediately and treat the prefix as possibly written.
   dc logs --no-log-prefix topup heartbeat | grep -F 'DATABASE_URL is required for'
   dc stop topup heartbeat backup migrate postgres
   dc rm -f topup heartbeat backup migrate postgres
   docker volume rm "$(dc config --format json | jq -r '.name')_pgdata"
   dc run --rm --no-deps --entrypoint /usr/bin/stat postgres \
     -c '%a %u %g' /run/wal-g/backup.key
   # Expected: 600 999 999
   ```

3. Choose a base backup by incident time. Listing is selection only. Read its version metadata and
   fetch it into the new empty PostgreSQL volume; this fetch is the decryption check. The `restore`
   service runs its argument with `/bin/sh -eu -c`, so pass each command as one quoted argument:

   ```sh
   dc run --rm --no-deps restore 'wal-g backup-list --json'
   export BACKUP_NAME=base_000000010000000000000003
   dc run --rm --no-deps restore "wal-g st cat key-versions/base/$BACKUP_NAME.json"
   dc run --rm --no-deps -e BACKUP_NAME="$BACKUP_NAME" restore '
     test -z "$(ls -A "$PGDATA")"
     walg-backup-fetch "$PGDATA" "$BACKUP_NAME"
     test -s "$PGDATA/PG_VERSION"
   '
   ```

4. Configure archive recovery. The wrapper returns a normal nonzero status only when WAL-G returns
   `74` (WAL genuinely absent), allowing recovery to end. Decryption, metadata, and storage errors
   return `126`, which makes PostgreSQL abort recovery instead of silently promoting:

   ```text
   restore_command = 'walg-restore-command %f %p'
   recovery_target_lsn = '0/5000060'
   recovery_target_inclusive = true
   recovery_target_action = 'promote'
   ```

   Omit `recovery_target_lsn` only when the incident decision is to replay every available WAL.
   Create `recovery.signal`, set `$PGDATA` mode `0700`, then start only PostgreSQL with
   `dc up -d --no-deps postgres` and require `SHOW archive_mode` to print `off` again.

5. Require `pg_isready` and `SELECT NOT pg_is_in_recovery()` to return true. Keep `topup`,
   `heartbeat`, and `backup` stopped.

6. Run the dedicated signer-enabled service. **Run it only while `topup`, `heartbeat`, and `backup`
   are stopped:** its post-restore reconciliation claims every deposit at or beyond `cleared` and
   adopts product answers, which must not race the service's own settlement pumps. It mounts the
   dstack socket, the attested route files, and uses owner credentials:

   ```sh
   dc run --rm --no-deps restore-check \
     topup restore-check \
     --expected-heartbeat-at "$EXPECTED_HEARTBEAT_AT" \
     --expected-lsn "$EXPECTED_LSN" \
     --route /etc/topup/routes/phala-cloud-sepolia-pha.yaml
   ```

   A staging drill stops after this step; see [Staging restore drill](#staging-restore-drill).

   `status` must be `ok`. The check verifies migration checksums, WAL state and distance, externally
   anchored RPO, and table counts, then runs the same library post-restore round as
   `topup reconcile --once --post-restore` (architecture section 13): every deposit in `cleared`,
   `credited`, or `swept`, plus deposits rejected by the product after reaching `cleared`, is queried
   by signed product GET and the product answer is adopted. A product lookup that fails, is not
   found, is still processing, fails identity verification, or would need an unsafe transition marks
   the round `incomplete`, is listed in `failures`, and exits nonzero. The regular reconciliation
   checks run in the same round; their findings and `failed_checks` (for example an unreachable
   chain RPC) are reported as alerts but do not gate resume.

7. **Real restore only: resume.** Compare incident markers and expected row counts first.
   Switching the encrypted environment to production values (`TOPUP_WAL_ARCHIVE=on`, read-write
   object-storage credentials, `DATABASE_URL`) restarts the app compose, and `app-compose.sh` brings
   `topup`, `heartbeat`, and `backup` up together on the restored database. The gateway routes the
   app's port 8080 to this CVM as soon as `topup` listens, so control ingress before the switch:
   confirm the failed instance is destroyed so only one CVM holds the application keys, and have the
   product owner hold calls to the service ([incident communication](runbooks/incident-communication.md)).
   Then switch the environment and immediately require:
   - `SHOW archive_mode` is `on`, and a new WAL segment on the promoted timeline appears with its
     `key-versions/wal/<segment>.json` object;
   - a fresh encrypted base backup (`dc exec -T backup walg-base-backup /var/lib/postgresql/data`);
   - `restore heartbeat recorded` log lines and healthy `topup` attestation;
   - `topup_backup_last_success_unixtime_seconds` scraped from the new instance and less than 120
     seconds old. Only then remove the restore-window alert silences.

   Let the product resume calls only after health and reconciliation remain clean. Addresses need no
   separate restore because their salts are deterministic from product data.

## Staging restore drill

The weekly staging drill restores the staging app's backups into a throwaway replacement CVM. It
follows [Authorize the replacement CVM](#authorize-the-replacement-cvm) and
[Restore the database](#restore-the-database) steps 1-6 with the
[restore-time environment](#replacement-cvm-boot-environment), and never leaves it. That CVM must
never write to the source WAL prefix: after promotion it creates a new timeline, and its `.history`
file, segments, and `key-versions/current.json` would make a later real restore follow
`recovery_target_timeline=latest` onto the drill's timeline. It must also never run `backup`
(`wal-g delete retain` on the shared prefix) or `topup` (the live application's keys, next to the
live instance).

The drill must run as an instance of the staging app itself: a staging-copy app id has a different
identity and cannot derive the staging backup keys. The dstack gateway can therefore route the
staging app's port-8080 traffic to the drill CVM, where `topup` keeps exiting on the empty
`DATABASE_URL`. Pause staging callers for the drill window and announce it; a request that still
reaches the drill CVM fails with a connection error and changes nothing.

1. Issue drill object-storage credentials that can only list and read `WALG_S3_PREFIX`, and put them
   with `TOPUP_WAL_ARCHIVE=off` and an empty `DATABASE_URL` in the drill's encrypted environment.
2. Silence `TopupBackupTooOld` and the scrape-target-down alert for the drill instance only (see
   [boot environment](#replacement-cvm-boot-environment)). Run steps 1-6 of the restore. Never run
   step 7 and never switch the drill's environment to production values. Record the
   `restore-check` report, RPO, and RTO in the drill log.
3. Destroy the drill CVM and its volumes, revoke the read-only drill credentials, and remove the
   drill silences.
4. **Finance Safe:** only if the recorded `DEVICE_PREVIOUSLY_ALLOWED` was `false` (the device was
   added for this drill), remove it with `removeDevice(bytes32)` and verify `allowedDeviceIds`
   returns `false`. Otherwise leave it: the live CVM may run on that host, and removing its device
   would stop it. Keep the compose hash; production uses the same attested compose. Resume the
   staging callers.

`deploy/local/restore-drill.sh` mirrors this: after destroying the source database it boots the
whole local stack with the restore-time environment, requires `topup` and `heartbeat` to fail closed,
archiving to be off, and the storage credentials to be read-only, stops and resets PostgreSQL as in
step 2, runs steps 3-6, and fails if the object-storage listing changed.

## Failure handling

- **App id, KMS, compose, or attestation mismatch:** stop before key derivation. Correct the original
  app authorization; never copy key files between CVMs.
- **Base metadata missing or decryption failure:** stop. Verify object-storage integrity and the
  retained version list. `backup-list` success is not evidence of a correct key.
- **WAL metadata spans versions:** keep all referenced `backup-vN.key` files mounted. The restore
  wrapper selects each segment's version; never rewrite old metadata to the current version.
- **WAL-G `74`:** recovery may end only when the requested target has been reached or the incident
  decision explicitly accepts replaying all available WAL.
- **Any other WAL error:** PostgreSQL must remain stopped. Preserve logs and test storage access,
  metadata, and decryption before retrying into another fresh volume.
- **`restore-check` incomplete:** do not resume. Product state wins, but an unsafe reverse transition
  or failed lookup requires an incident repair and another complete check.
- **RTO over 3600 seconds:** escalate even if the eventual restore passes.
