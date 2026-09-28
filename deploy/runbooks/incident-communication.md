# Incident communication

**Trigger:** any customer-impacting pause, credit delayed beyond policy, custody or reconciliation
mismatch, key compromise, restore, or material provider or price outage.

**Impact:** poor communication causes repeated deposits, support load, and unsafe pressure on
operators. State the affected routes, scopes, and customer effect; never publish keys, addresses
tied to a customer, raw payloads, or internal reason codes.

## First steps

1. Collect the facts: `curl -fsS "$BASE_URL/healthz"`, the daily report
   (`admin GET /v1/admin/reports/daily`), the open Sentry issues, and the scopes you paused.
2. **HUMAN-ONLY:** assign incident commander, operations lead, communications lead, and scribe;
   publish an initial status update within the organizational target.

## Decide

- No customer impact and recovered within threshold: internal event.
- Delayed quotes, addresses, credits, or refunds: public incident naming the route and scope.
- Custody, key, or reconciliation integrity risk: critical incident; involve Security, Finance,
  and Legal/Compliance before detailed claims.

## Fix

**HUMAN-ONLY:** each status update gives the start time in UTC, the affected route and network,
the customer effect, active pause scopes, what customers should do, and the next update time. Do
not estimate recovery until the owner accepts it.

## Done when

The technical runbook's exit criteria hold, every scope is intentionally resumed, Finance and
Support are briefed, and a final UTC timeline is recorded. A premature resolution is corrected at
once, with the incident reopened and its pauses re-applied.
