# Outbox backlog

## Trigger

Trigger on `TopupLoopStopped{loop="outbox"}`, growth of `topup_outbox_backlog` or
`topup_outbox_oldest_age_seconds` (PR #56 metrics without a dedicated alert), growing delivery
attempts, or product reports that notifications stopped.

## Impact and blast radius

Ledger and deposit states remain authoritative, but product notifications are delayed. One product
webhook or all products may be affected.

## First 5 minutes

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "BEGIN TRANSACTION READ ONLY; SELECT id,event_type,attempts,created_at,next_attempt_at,response FROM outbox WHERE delivered_at IS NULL ORDER BY created_at LIMIT 100; COMMIT;"
curl --fail-with-body -sS "$BASE_URL/healthz"
```

Check the affected product webhook endpoint, TLS, and receiver status without exposing payloads or
signing keys.

## Decision tree

- Receiver down/5xx: wait for recovery and preserve automatic backoff.
- 4xx/signature rejection: coordinate key/header verification before replay.
- Delivery succeeded but local row pending: investigate persistence before forcing replay.

## Remediation

Replay one stable webhook ID after the receiver is ready:

```sh
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup outbox replay --id "$EVENT_ID"
```

Replay a bounded time window only after counting it with SQL:

```sh
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup outbox replay --since "$SINCE_RFC3339"
```

Use `--force` only **HUMAN-ONLY** after the receiver confirms idempotent handling of already
delivered IDs.

## Verification

Pending age/count return to baseline, `delivered_at` is set, receiver logs show one logical event,
and no product balance changes result from replay.

## Rollback

Replay scheduling is not reversible. Stop further manual replay, let idempotency absorb duplicates,
and investigate the receiver before retrying.
