# Outbox backlog exercise

Date: 2026-09-22.

Status: partial; blocked on a seeded delivered event and a controllable webhook receiver.

G2 exercised once: [ ]

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T topup \
  topup outbox replay --since 2026-09-22T00:00:00Z
```

Observed:

```text
outbox replay scheduled count=0 force=false
outbox rows: 0
```

No receiver was configured, so delivered-event forced replay was intentionally not exercised.
