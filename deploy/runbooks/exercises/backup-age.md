# Backup age exercise

Date: 2026-09-22.

Status: blocked on D3 backup/restore automation in #58.

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

WAL archiving worked locally, but D3 encrypted base backup, age marker, and restore drill are absent.
This exercise remains partial and does not satisfy G2.
