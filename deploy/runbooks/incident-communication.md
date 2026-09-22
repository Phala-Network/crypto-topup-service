# Incident communication

## Trigger

Trigger for any customer-impacting pause, delayed credit beyond policy, custody/reconciliation
mismatch, key compromise, restore, or material provider/price outage.

## Impact and blast radius

Poor communication can cause repeated deposits, support load, and unsafe operator pressure. State
the affected routes/scopes and customer effect; never publish keys, addresses tied to a customer,
raw payloads, or internal reason codes.

## First 5 minutes

```sh
curl --fail-with-body -sS "$BASE_URL/healthz"
export NONCE="$(openssl rand -hex 32)"
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup attest --nonce "$NONCE" > /tmp/topup-attestation.json
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT state,count(*) FROM deposits GROUP BY state ORDER BY state; SELECT count(*) FILTER (WHERE delivered_at IS NULL) AS pending_outbox FROM outbox; SELECT route,paused_scopes FROM route_pauses ORDER BY route; COMMIT;"
```

**HUMAN-ONLY:** assign incident commander, operations lead, communications lead, and scribe. Publish
an initial status update within the organizational target.

## Decision tree

- No customer impact and self-recovered within threshold: internal event, no public incident.
- Delayed quotes/addresses/credits or refunds: public incident with affected route and scope.
- Custody, key, or reconciliation integrity risk: critical incident; involve Security, Finance,
  Legal/Compliance before detailed claims.

## Remediation

**HUMAN-ONLY:** status updates must include start time in UTC, affected route/network, observed
customer effect, active pause scopes, what customers should do, and next update time. Link the
technical runbook and incident ID. Do not estimate recovery until the owner accepts it.

## Verification

Before resolution, verify the technical runbook's exit criteria, all scopes intentionally resumed,
backlogs recovered, Finance/Support briefed, and a final UTC timeline recorded.

## Rollback

If a resolution message is premature, immediately post a correction, re-open the incident, restore
the prior severity, and reapply the relevant pause scopes.
