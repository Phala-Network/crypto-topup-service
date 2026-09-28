# Database migrations

Migrations run through the trusted database owner configured by `MIGRATE_DATABASE_URL`. The
`topup migrate` command never falls back to `DATABASE_URL`.

`20261004000000_multi_tenant` is the whole schema: docs/design/multi-tenant.md §14 on top of
docs/architecture.md §6. The pre-tenancy history (`20260922000000_initial_schema` through
`20261003000000_fast_credit`) was squashed into it without a data migration, because no
environment holding that history is kept: staging is reset (below) and production was never
deployed. It runs only on an empty database; on a database that still holds the old history the
migrator refuses to start, because the applied versions are missing from the binary. From here on
migrations are additive (plan §6 item 6): never edit an applied migration, and never squash again
once any environment holds data.

`20261005040000_chain_sourced_sweeps` (design PR 4) removes the operator flusher: `flushes`,
`flush_exclusions`, `deposits.flush_id`, the `flush` pause scope, and the treasury-inflow totals
of `reconciliation_custody_cursors`. It recreates `flushed` as the chain-sourced record of design
§14, adds `flush_failures` and `addresses.deployed_block`, and indexes credited deposits by
address for the sweep linkage.

**Staging reset, HUMAN-ONLY (design §16 PR 11).** An operator with the staging owner credentials
stops the service, drops and recreates the staging database (or restores an empty volume), runs
`topup migrate`, starts the service, and re-issues each account with `POST /v1/admin/accounts`
(`deploy/README.md`, Account credentials). Nothing is migrated: deposits, quotes, and events of the
old schema are discarded, and merchants take their new `acct_…` id and key id `{acct_…}/v1`.
Backups of the old database stay restorable only with a binary built before this migration.

The service runs through the login role configured by `DATABASE_URL`. That login role must be a
member of the migration-created `topup_app` NOLOGIN role. Table owners and PostgreSQL superusers
remain trusted migration/operations identities. `BEFORE UPDATE OR DELETE` triggers on
`transitions`, `audit`, and `reconciliation_findings` stay in place as defense in depth against
accidental owner-side mutation.

Default privileges grant `SELECT`, `INSERT`, `UPDATE`, and `DELETE` to `topup_app` on every table
the owner creates; no application table grants `TRUNCATE`. The migration narrows that grant:

| Tables | `topup_app` |
|---|---|
| `transitions`, `audit`, `reconciliation_findings`, `heartbeat` | `SELECT`, `INSERT` (append-only) |
| `flushed`, `flush_failures` | `SELECT`, `INSERT` (finalized chain facts) |
| `reconciliation_blocks` | `SELECT`, `INSERT`, `DELETE` |
| `reconciliation_deposit_cursors` | `SELECT`, `INSERT`, `UPDATE` |
| `_sqlx_migrations`, `permissions` | `SELECT` |
| every other table | `SELECT`, `INSERT`, `UPDATE`, `DELETE` |

A migration adding a table that should not get the full operational grant must narrow it in the
same migration. `topup_app` also has `USAGE, SELECT` on `heartbeat_id_seq`. The database test
`application_role_privileges_match_the_documented_grants` checks every `public` table against its
list, so a new table fails it until it is listed there and, if narrowed, here.

## Tenancy

Every tenant table (`customers`, `quotes`, `addresses`, `deposits`, `refunds`, `api_keys`,
`webhook_endpoints`, `events`, `idempotency_keys`, `account_limits`, and the account-owned
`confirmation_policies`, `treasuries`, `memberships`, `invitations`, `request_signing_keys`)
carries `account_id`, and the mode-bearing ones `livemode`. Merchant queries are built from a
server-side scope of both (`crate::tenancy::Scope`); the chain workers and the admin API act for the
platform and read across accounts. Composite foreign keys, `(parent_id, account_id, livemode)`
referencing a unique key of the parent, make a quote agree with its customer, an address with its
quote, a deposit with its address and customer, and a refund with its deposit, so no write can join
two accounts or two modes. `transitions`, `pending_transfers`, `flushed`, `flush_failures`,
`refund_payment_claims`, and `webhook_deliveries` have no `account_id` and are reached only
through their scoped parent.

`permissions` is the one authorization table (design D13): each row grants a permission to a role
(`role:owner`, `role:administrator`, `role:developer`, `role:view_only`) or an API key kind
(`key:secret`, `key:restricted`). The migration seeds it and the service can only read it.

## Kept until a later design PR

- `request_signing_keys` holds each account's RFC 9421 ed25519 key until API keys replace merchant
  request signing (design PR 5). Its key id is `{accounts.public_id}/v1`, and the key's `livemode`
  is the mode of every request it signs.
- The refund workflow columns (`requested_by`, `approved_by`, the `requested`, `approved`, `sent`,
  `confirmed` statuses, `to_address`) are the operator-approved flow design PR 9 replaces.
- `quotes.idempotency_key` and `refunds.idempotency_key` keep today's per-object replay until
  `idempotency_keys` serves every `POST` (design PR 5).
- Tables for API keys, treasuries, confirmation policies, account limits, and idempotency keys
  are created now and used by design PRs 5 and 7. The user, identity, passkey, recovery-code,
  membership, invitation, and session tables, the `role:*` principals, and the account profile,
  ToS, and `live_access` columns were created for a dashboard the design no longer has; design
  PR 5 drops them in an additive migration.

## Points the schema does not show on its own

- `accounts.public_id` is generated from `id`: `acct_` and its 32 hex digits.
- `addresses.treasury` is the forwarder's clone argument, the only address it can pay. Until
  treasuries are set per account (design PR 7) quotes take it from the route.
- `addresses.created_block` defaults to zero, which makes the first scanner pass check the full
  chain history before setting `backfilled`. Quote creation sets it from the chain's committed
  cursor instead.
- `deposits.confirmations_at` is when the transfer reached the required confirmation and was
  recorded; `final_at` when both providers showed it at `finalized`.
- `events.data` is `{}` until the first delivery attempt renders the object; it is never
  re-rendered, so every endpoint, retry, and replay sends the same body.
- `pending_transfers` is display-only, written by the head scan and cleared by the finalized
  scanner's cursor advance. Nothing that affects money reads it.
- The heartbeat RPO target is the code constant `topup::heartbeat::RPO_SECONDS`, not a column.
- A `chain` reconciliation block written by the address-derivation or the per-forwarder custody
  check freezes that chain at runtime: pumps leave its deposits waiting, its scanner pauses, and
  quote creation answers `409 chain_frozen`; the service keeps serving other chains. No check
  writes the `address` scope since the flusher is gone. An operator lifts a block with the
  admin-signed
  `POST /v1/admin/reconciliation-blocks/{block_key}/lift` and a `reason`: it deletes the row and
  writes an `audit` row carrying the removed block in one transaction.
