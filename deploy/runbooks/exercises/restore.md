# Restore exercise

Date: 2026-09-22.

Status: blocked on D3's `deploy/RESTORE.md` and implemented `topup restore-check` in #58.

G2 exercised once: [ ]

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T topup \
  topup restore-check
```

Observed:

```text
exit=1
restore-check is not implemented
```

`wal-g backup-list` also reported `No backups found`. D3 restore execution was therefore not
feasible and remains an explicit blocking gap.
