# Backup age exercise

Date: 2026-09-22.

Status: partial. The output below predates D3; D3 (#58) now provides encrypted base backups, WAL
key metadata, and `deploy/local/restore-drill.sh`, but this exercise has not been re-run against it.

G2 exercised once: [ ]

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T backup wal-g backup-list
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T postgres \
  psql -U postgres -d topup -At -c "SELECT archived_count,failed_count,last_archived_wal FROM pg_stat_archiver;"
```

Observed:

```text
No backups found
2|0|000000010000000000000003
```

WAL archiving worked locally before D3 landed. This exercise remains partial and does not satisfy G2
until it is re-run with D3 and the #56 alert.
