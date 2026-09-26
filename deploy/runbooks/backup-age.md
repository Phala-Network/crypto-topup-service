# Backup age

**Trigger:** the `topup-backup` monitor: three `error` check-ins in a row (the WAL-G success marker
is older than 120 seconds) or missed check-ins.

**Impact:** the service continues, but the recoverable point falls behind the one-minute RPO; a
database failure now would lose more data, on every route.

## First steps

1. During a restore the replacement archives nothing and runs no loop: expected, see
   [RESTORE.md](../RESTORE.md#the-restore-check-variant). A staging drill never affects this
   monitor.
2. List the newest archived segment with the owner's storage credentials:

   ```sh
   aws s3 ls "${WALG_S3_PREFIX%/}/wal_005/" --endpoint-url "$AWS_ENDPOINT" | tail -1
   ```

3. Check object storage health and the sealed credentials' permissions (R2 dashboard).

## Decide

- New segments keep arriving but the monitor is stale: the marker is not refreshed, or `topup`
  cannot read it; escalate to Engineering.
- No new segments and storage reachable: archiving or the heartbeat that forces one segment a
  minute has stopped, or the credentials lost write access. Fix the credentials and re-seal them
  ([deploy/README.md, "Sealing the secrets"](../README.md#sealing-the-secrets)); otherwise
  **HUMAN-ONLY:** restart the CVM (`npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"`).
- Storage unavailable: a provider outage; escalate and never delete WAL or backups.

## Done when

A segment younger than two minutes is listed, `topup-backup` checks in `ok`, and a later
[restore drill](../RESTORE.md#staging-restore-drill) passes within RPO and RTO. Never take an
unencrypted backup as a substitute.
