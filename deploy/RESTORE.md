# Backup and restore

Trigger: PostgreSQL loss or corruption, a failed database volume, or a restore drill. Targets: RPO
at most one minute, RTO at most one hour (architecture §14). While the database is unavailable
the API and processing stop for every route.

Restoring the database does not restore the business: the changes after the restore point are
lost, among them key revocations, treasury cancellations, endpoint deletions, deposit addresses
and quotes given to customers, and events merchants received. So a restored service starts
**frozen**: the admin API and `/healthz` work, every merchant request with an API key answers
`503 service_restoring` with `Retry-After`, reads included (a key revoked after the restore point
is valid in the restored database), and nothing credits, settles, or delivers an event until the
operator has sent every merchant's recorded contact the restore point, re-applied what each
reports (the key revocations first: the unfreeze is when keys authenticate again), and unfrozen it
([Reconciliation after a restore](runbooks/restore.md)).

A restore is a bootstrap from backup (the pattern of CloudNativePG's `bootstrap.recovery`): a new
instance of the same dstack app boots with an empty volume, and PostgreSQL itself fetches the
newest base backup, replays the archived WAL, and promotes. The instance boots the
[restore-check variant](#the-restore-check-variant), which verifies the result read-only; a real
restore then [resumes](#resume) by upgrading it to the service compose. No step runs inside a CVM.

## Backup key

WAL-G encrypts every base backup and WAL segment with one libsodium key per prefix. `keys`
derives it from the dstack path `backup/v1` and writes it only to the `walg_key` tmpfs
([README](README.md#database-credentials)); no key appears in an image, env, command line, object
metadata, or log. A replacement instance of the same app id derives the same key and database
passwords, so it needs no secret; a failed fetch or login means the app identity is wrong.

A prefix never changes its key. Rotation is a new prefix: one upgrade changes `BACKUP_KEY_DOMAIN`
(`crates/core/src/signer.rs`, for example to `backup/v2`) and `WALG_S3_PREFIX`; `backup` finds no
base backup of the running timeline in the new prefix and takes one at once. Keep the old prefix
until the new one holds `WALG_RETENTION_FULL` (7) base backups; until then it restores with the
restore-check variant rendered from the last commit before the change.

`wal-g backup-list` does not decrypt anything and is never a key test; only fetching a base backup
and reading its `PG_VERSION` is.

## Bootstrap from backup

On every boot of every instance, the PostgreSQL entrypoint
([postgres-walg-entrypoint.sh](scripts/postgres-walg-entrypoint.sh)) handles an empty data
directory from `WALG_S3_PREFIX`:

- If the prefix lists base backups, it fetches the newest beside the data directory, moves it into
  place only when complete, and PostgreSQL replays every archived segment and promotes.
- Only a successful, empty listing initializes a new cluster.
- Any listing error (storage unreachable, credentials not sealed yet or wrong, missing bucket)
  stops the container, which Docker restarts. It never initializes then, because a new cluster
  archiving into a prefix that holds a timeline would fork it. So a new CVM waits for its
  [sealed credentials](README.md#sealing-the-secrets).

A non-empty data directory is started as is; an interrupted fetch starts over and an interrupted
recovery resumes. A new app needs a prefix of its own: its key cannot decrypt another app's
backups. [tests/walg-archive-switch.sh](tests/walg-archive-switch.sh) covers these paths in CI.

`archive_timeout=60` and the `heartbeat` service (one commit a minute) archive a segment every
minute even when idle; after each successful upload `walg-cron` refreshes the marker that the
`topup-backup` Crons monitor watches ([README](README.md#sentry)).

## The restore-check variant

`deploy/render-compose.sh --restore-check` renders the same source with three attested
differences, so a verification instance has its own compose hash:

- `TOPUP_RESTORE_FROM_BACKUP=on`: a base backup is required (an empty prefix fails), archiving is
  off, and `backup` idles, so nothing writes to or deletes from the prefix.
- `TOPUP_SERVICE_ENABLED=read-only`: `topup` answers only `GET`, `HEAD`, and the operator's
  restore reconciliation under `/v1/admin/restore/` (anything else `503 service_restoring`, and so
  is every request with a merchant API key: `restore-check` records the freeze in parallel, and may
  fail before it does), runs
  no loop and takes no lease-owner lock, and reports to Sentry as `<environment>-restore`;
  `heartbeat` exits.
- No `dstack-ingress`: `topup` is published on 8081 instead
  ([why](#addressing-the-restore-check-instance)), and its origin is that gateway URL.

After PostgreSQL promotes (its health check passes only out of recovery; the start period is the
one-hour RTO) and `migrate` confirms the schema, `restore-check` runs once: it records the restore,
which freezes the service (the report's `restore_id`; the freeze is a row of the database, so it
holds after the upgrade to the service compose), then checks migration checksums, WAL state, row
counts, and runs a full reconciliation round on the restored ledger alone: the
service's record is authoritative for its credits, so nothing asks a merchant anything and the
check does not depend on any merchant being reachable (what merchants did after the restore point
is re-applied later, in the [reconciliation](runbooks/restore.md)). The read-only `topup` serves the report
on `/healthz`:

```json
{"mode":"read-only","restore_check":{"status":"ok","failures":[],"rpo_basis":"unanchored","restored_heartbeat_at":"…","latest_migration":…,"row_counts":{…},"post_restore_reconciliation":{"status":"complete","failed_checks":[],"findings":[…]},…}}
```

`restore_check` is `null` until the check finishes. A finding left unverified makes the status
`incomplete` and is listed in `failures`; a check that could not run reports `failed`. Other
reconciliation findings and `failed_checks` are reported but do not gate resume.

**Changes inside the RPO window.** A deposit credited in the last minute before the loss is
rebuilt from the chain by the rescan and credited again after the unfreeze, with the same deposit
id and `deposit.credited` event id. Before the unfreeze the operator imports the deliveries of the
events each merchant received after the restore point, and only those the service signed: the
rebuilt deposit then finds its event recorded and nothing is sent again with another body, and it
is valued at the credit the merchant was told, not re-valued, so its refunds and reversal reference
that credit. A deposit whose recorded transfer contradicts its delivered event is held until the
operator discards the delivered credit. A spot deposit whose delivery no merchant produces is
re-valued. The same reconciliation revokes again the keys, cancels again the treasury changes,
pauses or resumes treasury crediting again, and deletes again the endpoints that the restore
brought back, and re-issues the deposit addresses and quotes given out after the restore point,
identically; a re-issued quote's payment is credited at spot unless a signed delivery carries its
credit, and a quote no merchant reports stays lost ([runbook](runbooks/restore.md)).

A service that booted straight from backup into the service compose (an empty volume, so the
PostgreSQL entrypoint restored it, without the restore-check variant) is frozen as well: every
promotion out of archive recovery starts a new PostgreSQL timeline, and `topup run` freezes when
the timeline is newer than the one it acknowledged.

**Sentry during a real restore.** The replacement runs no loop and reports as
`<environment>-restore`, so every Crons monitor of the environment misses its check-ins and the
Uptime monitor fails. Mute the environment's Crons monitors and disable its Uptime monitor from
creation until [Resume](#resume) shows `topup-backup` checking in `ok`. A staging drill needs no
muting: the live instance keeps checking in.

### Render it and its env file

Render from the commit of the live compose, with the live images and every
[attested setting](README.md#attested-settings) exported (the derived ones too, with the values
Deploy derived: `SENTRY_ENVIRONMENT` the Environment's name, `AWS_REGION=auto` and
`AWS_S3_FORCE_PATH_STYLE=true` for R2, `TOPUP_ADMIN_KID=admin/<Environment>-v1`), but a
provisional `TOPUP_DOMAIN` (the instance's gateway host is known only after creation; the variant
needs no `TOPUP_GATEWAY_DOMAIN`):

```sh
export TOPUP_IMAGE=<live phala-pay digest> POSTGRES_WALG_IMAGE=<live postgres-walg digest>
TOPUP_DOMAIN=pending.invalid deploy/render-compose.sh --restore-check >restore-check.yml
```

The env holds the sealed names of [staging.env.example](staging.env.example), but with storage
credentials that can only list and read the prefix: on R2, an API token with **Object Read only**
on the backup bucket only. Create it on the operator's machine, for this restore only, and shred it
once the instance exists or the restore is abandoned:

```sh
umask 077
export RESTORE_ENV_DIR="$(mktemp -d)"
# One line per name of staging.env.example: every TOPUP_RPC_<ID>_KEY as well.
printf '%s\n' 'AWS_ACCESS_KEY_ID=<read-only key id>' 'AWS_SECRET_ACCESS_KEY=<read-only secret>' \
  'SENTRY_DSN=<the live DSN, or empty>' 'TOPUP_RPC_PROVIDER_A_KEY=<the live key, or empty>' \
  'TOPUP_RPC_PROVIDER_B_KEY=<the live key, or empty>' >"$RESTORE_ENV_DIR/restore.env"
deploy/preflight.sh --env "$RESTORE_ENV_DIR/restore.env" --compose restore-check.yml \
  --restore-check --offline
# after creating the instance:
shred -u "$RESTORE_ENV_DIR/restore.env" && rm -rf "$RESTORE_ENV_DIR"
```

## Addressing the restore-check instance

The dstack gateway routes `https://<app_id>-<port>.<gateway domain>` to any instance of the app
that accepts a connection on that port ([dstack usage](https://github.com/Dstack-TEE/dstack/blob/v0.5.9/docs/usage.md#access-the-app)).
Two instances listening on one port therefore share its traffic: Phala's first staging drill
(2026-09-25), when the service was still published on 8080, saw 8 of 12 live `/healthz` requests
reach its drill instance. The service now publishes only `dstack-ingress`, and the gateway sends
its [custom domain](README.md#custom-domain) to the one instance the domain's TXT record names.
The restore-check variant runs no ingress, so it never obtains a certificate for or answers on the
live domain, and publishes topup on 8081, which the service never does:

- `https://$TOPUP_DOMAIN` reaches only the live instance;
- `https://<app_id>-8081.<gateway domain>` (`RESTORE_URL`) reaches only the restore-check instance.

The live isolation check detects a failure; during a staging drill it is a hard abort:

```sh
# Every one of 20 requests must reach the service: an empty 200, never the read-only JSON or an error.
live_isolated() {
  for _ in $(seq 20); do
    test "$(curl -sS -o live-healthz.body -w '%{http_code}' "$LIVE_URL/healthz")" = 200 &&
      test ! -s live-healthz.body || return 1
  done
}
```

## Restore

**HUMAN-ONLY, owner**, with `PHALA_CLOUD_API_KEY` of the Environment exported.

1. **Create the instance.** It must be a new instance of the original app (same app id and KMS),
   never a new app, created with `restore-check.yml` and `--env-file` (without it Phala Cloud
   copies an existing instance's env, with read-write credentials). `--env-file` encrypts with the
   key of an existing instance record, so in a real restore stop the failed CVM but do not delete
   it before this step. `SOURCE_CVM_ID` is the Environment's `TOPUP_CVM_ID`:

   ```sh
   npx --yes phala@1.1.22 cvms get "$SOURCE_CVM_ID" --json >source.json
   export APP_ID="$(jq -er '.app_id' source.json)"
   npx --yes phala@1.1.22 instances add --app-id "$APP_ID" --compose-file restore-check.yml \
     --env-file "$RESTORE_ENV_DIR/restore.env" --name phala-pay-restore --json >instance.json
   export RESTORE_CVM_ID="$(jq -er '.vm_uuid' instance.json)"
   ```

2. **Verify its attestation**, addressing the guest agent by the instance id from the attested
   event log:

   ```sh
   npx --yes phala@1.1.22 cvms attestation "$RESTORE_CVM_ID" --json >attestation.json
   npx --yes phala@1.1.22 cvms get "$RESTORE_CVM_ID" --json >restore-cvm.json
   INSTANCE_ID="$(jq -er '[.tcb_info.event_log[] | select(.event == "instance-id")
     | .event_payload | ascii_downcase | select(test("^[0-9a-f]{40}$"))] | select(length == 1)[0]' \
     attestation.json)"
   curl -fsS "https://$INSTANCE_ID-8090.$(jq -er '.gateway.base_domain' restore-cvm.json)/prpc/Info" \
     >info.json
   deploy/verify-attestation.sh attestation.json info.json "$APP_ID" restore-check.yml
   ```

3. **Wait for the report** (at most the RTO) and require `.restore_check.status == "ok"`,
   `post_restore_reconciliation.status == "complete"`, the expected `latest_migration`, plausible
   `row_counts`, and `restored_heartbeat_at` at most 120 seconds older than the source point
   recorded outside the lost database (the failure time, or for a drill the creation time):

   ```sh
   export RESTORE_URL="https://${APP_ID#0x}-8081.$(jq -er '.gateway.base_domain' restore-cvm.json)"
   curl -fsS "$RESTORE_URL/healthz" | tee healthz.json | jq -e '.mode == "read-only" and .restore_check != null'
   ```

4. **Set its own origin**, so admin-signed requests verify (the admin API checks RFC 9421
   signatures against `TOPUP_PUBLIC_ORIGIN`, which the render derives from `TOPUP_DOMAIN`), and the
   reconciliation's steps 2 to 5 can run here: render the variant again with
   `TOPUP_DOMAIN` set to the host of `$RESTORE_URL` and upgrade this instance only (no `-e`, so
   its env stays). It restarts on its non-empty data directory and `restore-check` runs again:

   ```sh
   TOPUP_DOMAIN=${RESTORE_URL#https://} deploy/render-compose.sh --restore-check >restore-check.yml
   npx --yes phala@1.1.22 deploy --json --cvm-id "$RESTORE_CVM_ID" --compose restore-check.yml \
     --no-public-logs --no-public-sysinfo --wait
   ```

   Wait until `/healthz` is `ok` again.
5. **Verify the application identity** with a nonce-bound quote from `$RESTORE_URL`. Merchant
   keys are refused on this instance, so fetch it with the admin API (the
   [runbook environment](runbooks/README.md#environment) with `BASE_URL=$RESTORE_URL`), for any
   account of the Environment, and verify it exactly as `public-attestation.json` in
   [Attestation, ingress, and egress](README.md#attestation-ingress-and-egress), with the compose
   hash of the `restore-check.yml` rendered in step 4; the verified app id must be the original.
   Otherwise stop:

   ```sh
   export NONCE="$(openssl rand -hex 32)"
   admin GET "/v1/admin/attestation?account=$ACCOUNT&livemode=true&nonce=$NONCE" \
     > public-attestation.json
   ```

   Then the admin deposit view (`admin GET /v1/admin/deposits/{id}`) must return deposits known
   from before the loss in their recorded state, and `admin GET /v1/admin/restore` must show
   `"frozen": true` with the restore point.

### Resume

Real restore only, after a human review of the report, row counts, and incident markers, and
after steps 1 to 5 of the [reconciliation](runbooks/restore.md) (they run on this instance). Delete
the failed instance, so only one instance holds the keys and archives into the prefix; merchants
were sent the restore point in the reconciliation's step 2
([incident communication](runbooks/incident-communication.md)), and their API requests answer
`503 service_restoring` until the unfreeze. Then
render the service variant (`deploy/render-compose.sh`, no flag) with the Environment's settings
(its `TOPUP_DOMAIN`, the origin merchants call and admin requests are signed for), upgrade the instance to it (`phala deploy --cvm-id
"$RESTORE_CVM_ID" --compose <file>`, no `-e`), set `TOPUP_CVM_ID` to `$RESTORE_CVM_ID`, and seal
the read-write credentials (`phala envs update "$RESTORE_CVM_ID" -e <env file>`, the same three
names). The domain's TXT record still names the failed instance: set
`_dstack-app-address.$TOPUP_DOMAIN` to `$INSTANCE_ID:443` (step 2) so the gateway routes the
domain here and dstack-ingress, whose account and certificate volume is new, can obtain a
certificate; update a CAA record that pins the old ACME account. Require:

- `/healthz` at `https://$TOPUP_DOMAIN` answers `200` with an empty body, its [certificate
  evidence](README.md#custom-domain) verifies for the app id, and 8081 no longer answers;
- a `base_…` backup newer than the switch is listed and new segments of the promoted timeline
  appear under `wal_005/` (`backup` takes that base backup at once; until then the new timeline
  cannot be restored);
- a verified attestation and an `ok` check-in of `topup-backup`. Only then unmute the monitors.

The service comes up frozen: merchant requests with an API key answer `503 service_restoring`,
reads included, and the scanner rescans each chain from its restored cursor while nothing is
credited or delivered. Finish the [reconciliation](runbooks/restore.md) (steps 6 to 8): wait for
the rescan, check the delivered events against it, and unfreeze with the admin API (audited) only
once every key revoked after the restore point is revoked again; merchants' keys work again once it
is lifted. Tell every contact when the
service is back and confirm each merchant's keys, treasuries, endpoints, and deposit addresses are
as it left them.

If the restored state is wrong, keep the instance isolated: return traffic to the prior CVM only
if it is authoritative, otherwise restore again from an older verified backup and repeat the check.

## Staging restore drill

The drill restores an Environment's real backups (normally `staging`'s; an operator without a
`staging` Environment drills `production` the same way) into a throwaway instance of the same app (a
copy app cannot derive the backup key) and never goes past step 5 of [Restore](#restore). The
restore-check variant guarantees it never writes to the prefix (its promoted timeline would divert
a later real restore), never runs `backup` or the full `topup`, and never takes live traffic.

1. Issue a read-only token for the Environment's bucket, build the env file, render the variant
   with its live images and settings, and require that it publishes only 8081:
   `docker compose -f restore-check.yml config --format json | jq -e '[.services[] | .ports[]? | .published] == ["8081"]'`.
2. Require `live_isolated` to pass before the instance exists, with
   `LIVE_URL=https://$TOPUP_DOMAIN` (Phala's staging: `https://pay-api-staging.phala.com`).
3. Record the start time (the RPO anchor), run steps 1-5 of [Restore](#restore), and record the
   report, RPO, and RTO. **Hard abort:** run `live_isolated` right after creation, before each
   step, and at least every five minutes; if it fails once, delete the instance at once and record
   the drill as aborted with the failing responses.
4. Delete the instance by its own `vm_uuid` (never by app id or name, which also match the live
   instance) and revoke the token:

   ```sh
   npx --yes phala@1.1.22 cvms delete "$RESTORE_CVM_ID" --force
   ```

Never upgrade a drill instance to the service compose or seal read-write credentials into it.

## Local and CI drills

`make restore-drill` ([local/restore-drill.sh](local/restore-drill.sh)) runs two modes against
local object storage: `controlled` forces a WAL switch and requires the last marker and LSN;
`crash` kills PostgreSQL after a natural upload and reports the observed loss. Both require that a
wrong key fails the restore command (`126`), then boot the whole restore-check variant on an empty
volume with read-only credentials and require promotion with archiving off, an `ok` report with a
complete reconciliation, `503` on writes, the RPO and an RTO of at most 3600 seconds, and an
unchanged object listing. `controlled` also runs the business-consistency scenario: after the last
archived WAL, and before PostgreSQL is killed (a clean shutdown would archive them), the source
revokes an API key, rotates a customer's deposit address, and records a
delivered `deposit.credited`, and those writes are lost with the source; the merchant's records
also hold a quote created after the backup and its client secret. The drill requires that the
replacement is frozen (merchant writes and reads `503 service_restoring`, `GET /v1/admin/restore`
`frozen`), that [Restore](#restore) step 5's admin attestation answers where the merchant's does
not, with the account's webhook key, that the lost key is refused like every key while frozen and
is revoked again by prefix, that the lost address is re-issued with the same address and `da_` id,
that the quote is re-issued at its own address with its client secret (the payer's read works
again) while a secret of another quote is refused, that the delivered event's signed delivery is
imported exactly as delivered with no delivery and its credit kept for the deposit, while a body
changed after signing is refused and changes nothing, and that the unfreeze is refused while no
chain is rescanned. The [Restore drill](../.github/workflows/restore-drill.yml) workflow runs
it every Monday at 03:17 UTC and on demand; the CI `deployment` job runs the bounded WAL-G and
bootstrap tests on pull requests and pushes to `main`.

## Failure handling

- **App id, KMS, compose, or attestation mismatch:** stop and create the instance under the
  original app id again; never copy key files between CVMs.
- **`/healthz` never answers, or `restore_check` stays `null` past the RTO:** the backup could not
  be listed, fetched, or decrypted, or recovery failed (`walg-restore-command` returns `126` on
  decryption and storage errors, so PostgreSQL aborts instead of promoting). Delete the instance,
  check storage access and integrity with the owner's credentials, and retry with a new instance.
- **Backup list empty, stale, or unverifiable:** do not resume; escalate the data-loss risk.
- **`restore_check.status` is `failed` or `incomplete`:** do not resume. An unverified finding
  needs an incident repair and another complete check.
- **RTO over 3600 seconds:** escalate even if the restore then passes.
- **`live_isolated` fails during a drill:** delete the drill instance by its `vm_uuid`, confirm
  `live_isolated` passes again, and do not rerun until the rendered compose is confirmed to publish
  only 8081 and the gateway's routing is understood.
