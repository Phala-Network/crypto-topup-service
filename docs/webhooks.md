# Webhook integration

The service sends the event types listed in `docs/architecture.md` section 12 using Standard
Webhooks asymmetric signatures. Receivers must use the raw HTTP request body, not parsed and
re-serialized JSON, when verifying a signature.

Each request contains:

```text
webhook-id: <event UUID>
webhook-timestamp: <Unix seconds for this attempt>
webhook-signature: v1a,<base64 ed25519 signature>
```

The signed bytes are the exact concatenation
`webhook-id + "." + webhook-timestamp + "." + raw_body`. The event envelope is:

```json
{
  "event_id": "<same UUID as webhook-id>",
  "type": "deposit.confirmed",
  "created_at": "2026-09-22T12:00:00Z",
  "data": {}
}
```

A receiver must pin the service's settlement public key, accept the `v1a` ed25519 scheme, reject
timestamps outside its configured tolerance, and deduplicate successful processing by
`webhook-id`. Multiple space-separated signatures may appear during key rotation; accept the
request when one trusted signature verifies. Return any `2xx` status only after processing is
durable. The reference axum receiver in `crates/topup/tests/outbox.rs` demonstrates verification
against the untouched body bytes, timestamp checking, and idempotency-key extraction.
