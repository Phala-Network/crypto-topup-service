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
reports observed loss. Both pass the externally recorded source heartbeat and LSN to
`restore-check`, assert RTO is at most 3600 seconds, exercise a signed product GET, and remove their
uniquely named Compose projects and volumes. The full image build plus real 60-second archive window
is intentionally a weekly job; the bounded deployment CI job runs the WAL-G wrapper tests instead.

## Authorize the replacement CVM

A fresh CVM derives the same `backup/vN` key only when it runs under the original dstack application
identity: the same app id and KMS root, with an allowed attested compose. These are **HUMAN-ONLY**
steps using Phala Cloud credentials and the Finance Safe. Do not start `backup-key` yet.

1. Retrieve the original incident records and set the original `DstackApp` authorization contract:

   ```sh
   export ORIGINAL_APP_ID=0x<original-dstack-app-address>
   export APP_AUTH_CONTRACT="$ORIGINAL_APP_ID"
   export ORIGINAL_COMPOSE_HASH=0x<last-approved-compose-hash>
   cast call "$APP_AUTH_CONTRACT" 'owner()(address)' --rpc-url "$ETH_RPC_URL"
   ```

   Require the owner to be the recorded Finance Safe and the KMS chain/root to match the failed
   instance. Stop if the original app id or KMS root cannot be established.

2. Use the Phala replacement-instance workflow for that existing app id, not the new-application
   workflow. Prepare the exact retained compose and encrypted environment, then save the returned
   JSON as `prepare.json`. It must report the original app id:

   ```sh
   export REPLACEMENT_APP_ID="$(jq -er '.app_id' prepare.json)"
   test "${REPLACEMENT_APP_ID#0x}" = "${ORIGINAL_APP_ID#0x}"
   export COMPOSE_HASH="$(jq -er '.compose_hash' prepare.json)"
   export DEVICE_ID="$(jq -er '.device_id' prepare.json)"
   jq '{app_id, compose_hash, device_id, chain_id, onchain_status}' prepare.json
   ```

3. **Finance Safe:** authorize the replacement compose and device on the original contract. Submit
   the generated calldata through the Safe, wait for finality, then verify both reads return `true`:

   ```sh
   cast calldata 'addComposeHash(bytes32)' "$COMPOSE_HASH"
   cast calldata 'addDevice(bytes32)' "$DEVICE_ID"
   cast call "$APP_AUTH_CONTRACT" 'allowedComposeHashes(bytes32)(bool)' "$COMPOSE_HASH" \
     --rpc-url "$ETH_RPC_URL"
   cast call "$APP_AUTH_CONTRACT" 'allowedDeviceIds(bytes32)(bool)' "$DEVICE_ID" \
     --rpc-url "$ETH_RPC_URL"
   ```

4. Commit the prepared replacement only after both authorizations are final. Fetch `cvm.json` and
   `attestation.json`, then run the normal compose verification:

   ```sh
   deploy/verify-attested-compose.sh \
     attestation.json cvm.json deploy/docker-compose.yml
   ```

5. Request a fresh application-bound quote inside the replacement CVM. Verify the quote, TCB, RTMR
   event log, KMS chain, and compose through the official dstack verification flow. The CLI includes
   dstack's reported app id; compare it to the original before deriving any backup key:

   ```sh
   export NONCE="$(openssl rand -hex 32)"
   docker compose run --rm --no-deps topup topup attest --nonce "$NONCE" > restore-attestation.json
   jq -e --arg app "${ORIGINAL_APP_ID,,}" \
     '(.app_id | ascii_downcase) == $app and (.quote | length > 0)' \
     restore-attestation.json
   ```

   A mismatched or unverifiable app id means this instance cannot be trusted to derive the original
   backup key. Stop; do not try decryption and do not create a new key under an old version number.

## Restore the database

1. Record the source failure point outside the destroyed PostgreSQL volume. Use the last committed
   heartbeat timestamp and source WAL insert LSN from incident monitoring:

   ```sh
   export EXPECTED_HEARTBEAT_AT=2026-09-22T14:35:18.172465Z
   export EXPECTED_LSN=0/5000000
   ```

   If either value is unavailable, the RPO cannot be proven. Continue only as an explicitly declared
   incident exception and keep service traffic stopped.

2. Configure the current and retained key versions, start only the key service, and verify metadata,
   never contents:

   ```sh
   export TOPUP_BACKUP_KEY_VERSION=1
   export TOPUP_BACKUP_KEY_FALLBACK_VERSIONS=0
   docker compose up -d backup-key
   docker compose run --rm --no-deps --entrypoint /usr/bin/stat postgres \
     -c '%a %u %g' /run/wal-g/backup.key
   # Expected: 600 999 999
   ```

3. Choose a base backup by incident time. Listing is selection only. Read its version metadata and
   fetch it into a new empty PostgreSQL volume; this fetch is the decryption check:

   ```sh
   docker compose run --rm --no-deps restore wal-g backup-list --json
   export BACKUP_NAME=base_000000010000000000000003
   docker compose run --rm --no-deps restore \
     wal-g st cat "key-versions/base/$BACKUP_NAME.json"
   docker compose run --rm --no-deps -e BACKUP_NAME restore '
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
   recovery_target_lsn = '0/5000000'
   recovery_target_inclusive = true
   recovery_target_action = 'promote'
   ```

   Omit `recovery_target_lsn` only when the incident decision is to replay every available WAL.
   Create `recovery.signal`, set `$PGDATA` mode `0700`, then start only PostgreSQL.

5. Require `pg_isready` and `SELECT NOT pg_is_in_recovery()` to return true. Keep the public service,
   heartbeat, and backup processes stopped.

6. Run the dedicated signer-enabled service. It mounts the dstack socket and uses owner credentials;
   every deposit in `cleared`, `credited`, or `swept`, plus deposits rejected by the product after
   reaching `cleared`, is queried by signed product GET, including settlements already stored as
   accepted or rejected:

   ```sh
   docker compose run --rm --no-deps restore-check \
     topup restore-check \
     --expected-heartbeat-at "$EXPECTED_HEARTBEAT_AT" \
     --expected-lsn "$EXPECTED_LSN"
   ```

   `status` must be `ok`. The check verifies migration checksums, WAL state and distance, externally
   anchored RPO, table counts, product identity fields, authoritative pricing, settlement receipts,
   deposit transitions, and outbox events. `processing`, not found, transport failure, protocol
   failure, unsafe state reversal, or missing implementation is reported as `incomplete` and exits
   nonzero. The product answer is authoritative; local payload equality is not required.

7. Compare incident markers and expected row counts. Start `heartbeat` and `backup`, require a new
   WAL segment and its `key-versions/wal/<segment>.json` object, then start `topup` without ingress.
   Enable ingress only after health and reconciliation remain clean. Addresses need no separate
   restore because their salts are deterministic from product data.

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
