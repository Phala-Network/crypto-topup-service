# Backup and restore runbook

This runbook implements architecture sections 13-15. A restore is a bootstrap from backup, the
pattern of CloudNativePG's `bootstrap.recovery` and Spilo's clone from backup: a new instance of
the same dstack application boots with an empty database volume, and PostgreSQL itself fetches the
newest base backup, replays every archived WAL segment, and promotes
([Bootstrap from backup](#bootstrap-from-backup)). The instance boots the
[restore-check variant](#the-restore-check-variant) of the attested compose, which verifies the
restore read-only; a real restore then resumes by upgrading that instance to the service compose.
Production OS images have no SSH or logs, so no step runs inside the CVM: the operator renders the
compose, creates the instance, and verifies it only through its `/healthz` and read API on port
8081 of the app's gateway URL, which only a restore-check instance serves
([Addressing the restore-check instance](#addressing-the-restore-check-instance)).

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

Both modes start the source from an empty object store, so its PostgreSQL must list the prefix and
initialize a new cluster. They then destroy the source database and its key volumes and boot the
whole [restore-check variant](#the-restore-check-variant) at once, as dstack does, with read-only
storage credentials. They require that PostgreSQL restored the newest base backup and promoted
with `archive_mode=off`, that `heartbeat`
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
restore-command, and bootstrap and archive switch tests on the self-hosted runner. The CI runner
reaches the host Docker daemon through its socket and the daemon cannot see the checkout, so the
drill never bind-mounts a host path: `deploy/local/restore-drill.compose.yml` swaps the local
stack's file mounts for project volumes that the drill fills with `docker cp`, locally and on CI
alike.

## Bootstrap from backup

The PostgreSQL entrypoint (`deploy/scripts/postgres-walg-entrypoint.sh`) bootstraps an empty data
directory on every boot of every instance, from `WALG_S3_PREFIX`:

- It lists the base backups (`wal-g backup-list --json`). If the prefix holds any, it selects the
  newest (latest `time`), fetches it with `walg-backup-fetch` beside the data directory, and moves
  it into place only when complete, with `recovery.signal`. PostgreSQL then replays every archived
  segment through `walg-restore-command` and promotes at the end of the archive.
- Only a listing that succeeds and is empty (`[]`) lets PostgreSQL initialize a new cluster.
- Any listing error (object storage unreachable, credentials not sealed yet or wrong, a missing
  bucket) stops the container, which Docker restarts; it never initializes a cluster then,
  because a new cluster archiving into a prefix that holds a timeline would fork it. A new CVM
  therefore waits for its sealed storage credentials ([README](README.md#deploy) step 5).

A data directory that holds anything is started as is and never overwritten; an interrupted fetch
starts over and an interrupted recovery resumes (`restore_command` is always set; PostgreSQL uses
it only in recovery). A new app needs a prefix of its own: its keys cannot decrypt another app's
backups, so the fetch fails and the instance stays down. `deploy/tests/walg-archive-switch.sh`
verifies these paths on the image.

## The restore-check variant

`deploy/render-compose.sh --restore-check` renders the same source as the service with three
attested differences, so a verification instance is identifiable by its compose hash:

- `TOPUP_RESTORE_FROM_BACKUP=on`. PostgreSQL requires a base backup (an empty prefix fails like a
  listing error) and never archives: archiving is off whatever flags are passed, `walg-cron`
  refuses `wal-push`, and `backup` idles (`base backups are disabled while
  TOPUP_RESTORE_FROM_BACKUP=on`), so neither a base backup nor `wal-g delete retain` touches the
  prefix. `restore-check` runs (below).
- `TOPUP_SERVICE_ENABLED=read-only`: `topup run` serves only `GET` and `HEAD` requests (anything
  else answers `503`) and runs no scanner, pump, flusher, webhook delivery, reconciler, or
  lease-owner lock; it skips the startup contract check because it issues nothing. `heartbeat`
  exits at its configuration check (`heartbeat is disabled while TOPUP_SERVICE_ENABLED=read-only`).
  With `SENTRY_DSN` set, it reports errors under the Sentry environment `<environment>-restore`,
  never as the live environment, and sends no Crons check-in because no loop runs
  ([deploy/README.md, "Sentry"](README.md#sentry)).
- `topup` is published on port 8081 (`8081:8080`) instead of the service's 8080, so the gateway
  never sends the live URL's traffic to it
  ([Addressing the restore-check instance](#addressing-the-restore-check-instance)).

Every other setting is the service's, rendered from the same values (for staging, the `staging`
Environment variables), including `TOPUP_BACKUP_KEY_VERSION` and
`TOPUP_BACKUP_KEY_FALLBACK_VERSIONS` (current version first, then every retained version), except
`TOPUP_PUBLIC_ORIGIN=https://pending.invalid` at creation: the gateway domain of the node that
runs the instance is known only afterwards
([Verify the restored instance](#verify-the-restored-instance)). Render it from the
commit of the live compose, with the live images and every attested setting
([README](README.md#attested-settings)) exported:

```sh
export TOPUP_IMAGE=<live crypto-topup digest> POSTGRES_WALG_IMAGE=<live postgres-walg digest>
TOPUP_PUBLIC_ORIGIN=https://pending.invalid deploy/render-compose.sh --restore-check >restore-check.yml
deploy/preflight.sh --env "$RESTORE_ENV_DIR/restore.env" --compose restore-check.yml \
  --restore-check --offline
```

After PostgreSQL has promoted (its health check passes only once it is out of recovery; the start
period allows the one-hour RTO) and `migrate` has confirmed the schema, the `restore-check` service
runs once. It verifies migration checksums, WAL state, and table counts, then runs the
post-restore round (architecture section 13): every deposit in
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
example an unreachable chain RPC) are reported but do not gate resume. In the service variant,
`restore-check` exits at once on every boot.

Alerts during the restore window are expected, not incidents. The replacement archives nothing
and refreshes no backup-age marker, so `TopupBackupTooOld` fires whenever an old or missing marker
is scraped, and the read-only `topup` exports no metrics, so the monitoring collector's
scrape-target-down alert for the instance fires. Silence both, scoped to the replacement instance,
from creation until [Resume](#resume-real-restore-only) shows a fresh archived segment and the
marker is younger than 120 seconds. During a staging drill, silence them for the drill instance
only, never for the live staging instance, and remove the silences when the drill instance is
deleted.

### The restore env file

The env holds only the owner-sealed secrets ([staging.env.example](staging.env.example)); every
setting is in the rendered compose. `restore.env` holds object-storage credentials that can only
list and read `WALG_S3_PREFIX`, so a mistake cannot write, overwrite, or delete backup objects. On
Cloudflare R2 this is an R2 API token with the permission **Object Read only**, scoped to the
backup bucket only (staging: `crypto-topup-test`). Create it only on the operator's machine, in a
private directory, and only for this restore:

```sh
umask 077
export RESTORE_ENV_DIR="$(mktemp -d)"
printf '%s\n' 'AWS_ACCESS_KEY_ID=<read-only restore key id>' \
  'AWS_SECRET_ACCESS_KEY=<read-only restore secret>' 'SENTRY_DSN=<the live DSN, or empty>' \
  >"$RESTORE_ENV_DIR/restore.env"
```

The names must be exactly the application's `allowed_envs`, which the compose hash fixes
(`preflight.sh` above checks them). Delete the file when the instance exists, and also when the
restore is abandoned:

```sh
shred -u "$RESTORE_ENV_DIR/restore.env"
rm -rf "$RESTORE_ENV_DIR"
```

## Create the replacement instance

A replacement derives the same `backup/vN` keys and database passwords only under the original
dstack application identity: the same app id and KMS, with an allowed attested compose. It is a new
instance of that app, never a new app, created with the `restore-check.yml` rendered
[above](#the-restore-check-variant). `phala instances add` accepts a compose of its own for the new
instance (CLI 1.1.22 `--compose-file`, sent as the request's `docker_compose_file`; the SDK's
`createAppInstance`: "Deploy a new CVM instance under an existing app, optionally with a new Docker
Compose file"). Never create it with the live compose revision (`--compose-hash`, or neither
option): that is the service variant, which would restore and then archive, run `backup`, and run
the full `topup` next to the live instance. Pass `--env-file` on every create: without it, Phala
Cloud gives the new instance the encrypted environment of an existing instance of the app, with
its read-write storage credentials.

### Addressing the restore-check instance

The dstack gateway routes `https://<id>-<port>.<gateway.base_domain>` to port `<port>` in a CVM,
where `<id>` is an app id or an instance id ([dstack usage guide][dstack-usage]: "When using the
app ID, the load balancer will select one of the available instances"). For an app id, the gateway
(`select_top_n_hosts` in [gateway v0.5.9][gw-select] and [v0.6.0-rc5][gw-select-06]) takes up to
`connect_top_n` instances of the app (3 in the shipped configuration) with a WireGuard handshake,
opens a TCP connection to each at once, and proxies to the first that connects; an instance whose
connection fails is skipped (`connect_multiple_hosts`, [v0.5.9][gw-connect],
[v0.6.0-rc5][gw-connect-06]). Two instances of the app listening on the same port therefore share
its traffic. The first staging drill (2026-09-25) observed exactly that: while its restore-check
instance published 8080 like the service, 8 of 12 `/healthz` requests to the live URL
`https://<app_id>-8080.<gateway.base_domain>` reached the drill instance. The instance-id form
routes to one instance, but Phala Cloud reports `instance_id` in `cvms get` only once it has
recorded it (the SDK's [`refreshCvmInstanceId`][phala-refresh] backfills it from the node or the
gateway; CLI 1.1.22 has no command for it), and the drill instance's stayed `null` throughout. The
instance id itself is in the instance's attested event log (the `instance-id` event), so steps
that need it read it there.

The restore-check variant therefore publishes `topup` on 8081, a port the service never publishes:

- `https://<app_id>-8080.<gateway.base_domain>` reaches only instances running the service: a
  restore-check instance does not accept a connection on 8080, so the gateway completes the
  connection to the live instance instead.
- `https://<app_id>-8081.<gateway.base_domain>` (`RESTORE_URL`) reaches only the restore-check
  instance, for the same reason. It is fixed by the app id and the gateway domain alone.

This rests on the gateway trying more than one instance per connection. The gateway operator can
configure `connect_top_n`; if it tried one instance only, requests to the live URL that picked the
restore-check instance would fail instead. The live isolation check below detects both failures,
and during a staging drill it is a hard abort:

```sh
# Every one of 20 requests must reach the service: an empty 200, never the read-only JSON or an error.
live_isolated() {
  for _ in $(seq 20); do
    test "$(curl -sS -o live-healthz.body -w '%{http_code}' "$LIVE_URL/healthz")" = 200 &&
      test ! -s live-healthz.body || return 1
  done
}
```

In a real restore the failed instance is stopped or deleted, so no live instance shares the app's
gateway routes; the replacement is verified on 8081 and serves 8080 only after
[Resume](#resume-real-restore-only) upgrades it to the service compose, and products hold their
calls until then. The procedure is otherwise unchanged.

[dstack-usage]: https://github.com/Dstack-TEE/dstack/blob/v0.5.9/docs/usage.md#access-the-app
[gw-select]: https://github.com/Dstack-TEE/dstack/blob/v0.5.9/gateway/src/main_service.rs#L1084-L1144
[gw-select-06]: https://github.com/Dstack-TEE/dstack/blob/gateway-v0.6.0-rc5/dstack/gateway/src/main_service.rs#L2359-L2434
[gw-connect]: https://github.com/Dstack-TEE/dstack/blob/v0.5.9/gateway/src/proxy/tls_passthough.rs#L144-L183
[gw-connect-06]: https://github.com/Dstack-TEE/dstack/blob/gateway-v0.6.0-rc5/dstack/gateway/src/proxy/tls_passthough.rs#L214-L277
[phala-refresh]: https://github.com/Phala-Network/phala-cloud/blob/cli-v1.1.22/js/src/actions/cvms/refresh_cvm_instance_id.ts

### Phala Cloud KMS (staging)

With `--kms phala` there is no on-chain authorization: `phala instances add` creates and boots the
instance in one call (CLI 1.1.22, `cli/src/commands/instances/add`). Record the returned
`vm_uuid`:

```sh
npx --yes phala@1.1.22 cvms get "$SOURCE_CVM_ID" --json >source.json
export APP_ID="$(jq -er '.app_id' source.json)"
npx --yes phala@1.1.22 instances add --app-id "$APP_ID" --compose-file restore-check.yml \
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
   instance. `ORIGINAL_COMPOSE_HASH` is the live service compose, needed again to
   [resume](#resume-real-restore-only). Stop if the original app id or KMS root cannot be established.

2. Prepare a new instance of that existing app with `phala instances add`, not the new-application
   workflow; `phala cvms replicate` needs the source CVM, which may be gone. The commands and JSON
   paths below match phala CLI 1.1.22. Pass the `restore-check.yml` rendered from the retained
   production compose's commit, images, and settings, and
   [the restore env file](#the-restore-env-file). `--env-file` encrypts it with the key of an
   existing instance record of the app, so stop the failed CVM but do not delete it before this
   step. `NODE_ID` is the numeric node (teepod) id to run the replacement on:

   ```sh
   npx --yes phala@1.1.22 nodes list --json
   export NODE_ID=<node id>
   npx --yes phala@1.1.22 instances add --app-id "$ORIGINAL_APP_ID" --node-id "$NODE_ID" \
     --compose-file restore-check.yml --env-file "$RESTORE_ENV_DIR/restore.env" \
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

   Fetch `attestation.json` and the instance's guest-agent `info.json` (as in the deploy
   read-back, [README](README.md#a-first-time-provisioning)), addressed by the instance id from the
   attested event log, because `cvms get` may report `instance_id` as `null`
   ([Addressing the restore-check instance](#addressing-the-restore-check-instance)). Then verify
   the attestation with the original app id; it replays that event log:

   ```sh
   npx --yes phala@1.1.22 cvms attestation "$RESTORE_CVM_ID" --json >attestation.json
   npx --yes phala@1.1.22 cvms get "$RESTORE_CVM_ID" --json >restore-cvm.json
   INSTANCE_ID="$(jq -er '[.tcb_info.event_log[] | select(.event == "instance-id")
     | .event_payload | ascii_downcase | select(test("^[0-9a-f]{40}$"))] | select(length == 1)[0]' \
     attestation.json)"
   curl -fsS "https://$INSTANCE_ID-8090.$(jq -er '.gateway.base_domain' restore-cvm.json)/prpc/Info" \
     >info.json
   deploy/verify-attestation.sh \
     attestation.json info.json "$ORIGINAL_APP_ID" restore-check.yml
   ```

## Verify the restored instance

1. Derive the restore-check URL, port 8081 of the app on the gateway of the instance's node
   ([Addressing the restore-check instance](#addressing-the-restore-check-instance)), and wait (at
   most the one-hour RTO) until `/healthz` serves a report:

   ```sh
   npx --yes phala@1.1.22 cvms get "$RESTORE_CVM_ID" --json >restore-cvm.json
   RESTORE_APP_ID="$(jq -er '.app_id' restore-cvm.json)" || exit 1
   export RESTORE_URL="https://${RESTORE_APP_ID#0x}-8081.$(jq -er '.gateway.base_domain' restore-cvm.json)"
   curl -fsS "$RESTORE_URL/healthz" | tee healthz.json | jq -e '.mode == "read-only" and .restore_check != null'
   ```

2. Require `.restore_check.status == "ok"`, `.restore_check.post_restore_reconciliation.status ==
   "complete"`, the expected `latest_migration`, plausible `row_counts`, and, against the source
   point recorded outside the lost database (the time of the failure, or for a drill the time the
   instance was created), `restored_heartbeat_at` at most 120 seconds older (RPO 60 seconds plus
   one heartbeat interval).

3. Request an application-bound quote with a fresh nonce and verify it with the official dstack
   verifier, as in [Attestation, ingress, and egress](README.md#attestation-ingress-and-egress)
   (quote and TCB, RTMR3 replay, OS image). The verified app id must be the original; a mismatch
   or an invalid result means this instance does not hold the application identity. Stop.

   ```sh
   export NONCE="$(openssl rand -hex 32)"
   curl -fsS "$RESTORE_URL/v1/attestation?nonce=$NONCE" > restore-attestation.json
   jq '{quote: null, attestation: .quote}' restore-attestation.json |
     deploy/dstack-verifier.sh > restore-verification.json
   jq -e --arg app "$(printf '%s' "${APP_ID#0x}" | tr 'A-F' 'a-f')" \
     '.details.tcb_status == "UpToDate" and .details.app_info.app_id == $app' \
     restore-verification.json
   ```

4. Signed product requests carry the URL they were sent to in `@target-uri`, so set the instance's
   own URL as its origin: render the restore-check variant again with
   `TOPUP_PUBLIC_ORIGIN=$RESTORE_URL` and upgrade this instance only (no `-e`, so its env stays;
   with on-chain KMS, approve the returned hash as in [README](README.md#b-upgrade-an-existing-cvm)
   section B). It restarts; its data directory is no longer empty, so it starts as is, and
   `restore-check` runs again:

   ```sh
   TOPUP_PUBLIC_ORIGIN=$RESTORE_URL deploy/render-compose.sh --restore-check >restore-check.yml
   npx --yes phala@1.1.22 deploy --json --cvm-id "$RESTORE_CVM_ID" --compose restore-check.yml \
     --no-public-logs --no-public-sysinfo --wait
   ```

   Wait for `/healthz` to serve an `ok` report again, then check known deposits with
   product-signed support lookups, for example with the Python SDK and the product's key:
   `TopupClient("$RESTORE_URL", "<product slug>", signer).lookup_deposits(tx_hash=...)` must
   return each deposit in its recorded state.

## Resume (real restore only)

Compare incident markers and expected row counts first. Resuming upgrades the verified instance to
the service compose, which restarts it on the restored database, which is then started as is.
Control ingress before the switch: confirm the failed instance is deleted so only one instance
holds the application keys and archives into the prefix, and have the product owner hold calls to
the service ([incident communication](runbooks/incident-communication.md)). Then render the service
variant (`deploy/render-compose.sh`, no `--restore-check`) with the production settings and the
origin products will call, upgrade the instance to it (staging: `phala deploy --cvm-id
"$RESTORE_CVM_ID" --compose <file>`, no `-e`; production: README section B), and seal the
read-write object-storage credentials (`phala envs update "$RESTORE_CVM_ID" -e <env file>`, the
same three names). Require:

- `/healthz` on the service URL `https://<app_id>-8080.<gateway.base_domain>` answers `200` with
  an empty body (the full service, not the read-only one; `RESTORE_URL` on 8081 no longer
  answers);
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
to the live instance). The restore-check variant guarantees all three, and it never receives the
live URL's traffic because it publishes 8081, not 8080
([Addressing the restore-check instance](#addressing-the-restore-check-instance)).

1. Issue an R2 API token with **Object Read only** on the staging bucket only, build the
   [restore env file](#the-restore-env-file) with it, and render the
   [restore-check variant](#the-restore-check-variant) with the live staging images and settings.
   Require that it publishes only `topup` on 8081 (the preflight's fresh render enforces it too):

   ```sh
   docker compose -f restore-check.yml config --format json |
     jq -e '[.services[] | .ports[]? | .published] == ["8081"]'
   ```

2. Derive the live URL and require the [live isolation check](#addressing-the-restore-check-instance)
   (`live_isolated`) to pass before the drill instance exists:

   ```sh
   npx --yes phala@1.1.22 cvms get "$SOURCE_CVM_ID" --json >source.json
   LIVE_APP_ID="$(jq -er '.app_id' source.json)" || exit 1
   export LIVE_URL="https://${LIVE_APP_ID#0x}-8080.$(jq -er '.gateway.base_domain' source.json)"
   live_isolated
   ```

3. Silence `TopupBackupTooOld` and the scrape-target-down alert for the drill instance only.
4. Record the drill start time (the RPO anchor), then
   [create the instance](#phala-cloud-kms-staging) and delete the env file.
5. Run [Verify the restored instance](#verify-the-restored-instance) and record the report, RPO,
   and RTO in the drill log. Never upgrade the drill instance to the service compose or seal
   read-write credentials into it. **Hard abort:** run `live_isolated` right after the instance is
   created, then at least every five minutes and before each verification step (including after
   the upgrade in its step 4) until the instance is deleted. If it fails even once, stop the drill,
   delete the drill instance at once (next step), and record the drill as aborted with the failing
   responses.
6. Delete the drill instance by its own `vm_uuid` (never by app id or name, which also match the
   live instance), revoke the read-only token, and remove the drill silences:

   ```sh
   npx --yes phala@1.1.22 cvms delete "$RESTORE_CVM_ID" --force
   ```

`deploy/local/restore-drill.sh` mirrors this against local object storage.

## Failure handling

- **App id, KMS, compose, or attestation mismatch:** stop. Correct the original app authorization;
  never copy key files between CVMs.
- **`/healthz` never answers, or `restore_check` stays `null` past the RTO:** the base backup
  could not be listed (or the prefix is empty), fetched, or decrypted, or recovery failed (the WAL wrapper returns `126` on
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
- **`live_isolated` fails during a staging drill** (the live URL answered with the read-only JSON,
  an error, or no response): delete the drill instance at once by its `vm_uuid`, confirm
  `live_isolated` passes again, and do not rerun the drill until the rendered restore-check compose
  is confirmed to publish only 8081 and the gateway's routing is understood.
