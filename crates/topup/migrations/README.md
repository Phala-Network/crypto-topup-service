# Database migrations

Migrations run through the trusted database owner configured by `MIGRATE_DATABASE_URL`. The
`topup migrate` command never falls back to `DATABASE_URL`.

`20260922000000_initial_schema` is the whole schema of docs/architecture.md §6. Before any
environment held data, the pre-pilot migration history was squashed into it; its version sorts
first so later migrations apply after it. From here on migrations are additive (plan §6 item 6):
never edit an applied migration, and never squash again once any environment holds data.

The service runs through the login role configured by `DATABASE_URL`. That login role must be a
member of the migration-created `topup_app` NOLOGIN role. Table owners and PostgreSQL superusers
remain trusted migration/operations identities. `BEFORE UPDATE OR DELETE` triggers on
`transitions`, `audit`, and `reconciliation_findings` stay in place as defense in depth against
accidental owner-side mutation.

Default privileges grant `SELECT`, `INSERT`, `UPDATE`, and `DELETE` to `topup_app` on every table
the owner creates; no application table grants `TRUNCATE`. The initial schema narrows that grant,
`20260927000000_admin_ops` adds `DELETE` on `reconciliation_blocks` for the admin lift, and
`20260928000000_webhook_fulfillment` makes `settlements` read-only history:

| Tables | `topup_app` |
|---|---|
| `transitions`, `audit`, `reconciliation_findings`, `heartbeat` | `SELECT`, `INSERT` (append-only) |
| `reconciliation_blocks` | `SELECT`, `INSERT`, `DELETE` |
| `reconciliation_deposit_cursors`, `reconciliation_custody_cursors` | `SELECT`, `INSERT`, `UPDATE` |
| `_sqlx_migrations`, `settlements` | `SELECT` |
| `products`, `accounts`, `route_pauses`, `seen_signatures`, `addresses`, `cursors`, `pending_transfers`, `flushes`, `flushed`, `flush_exclusions`, `deposits`, `rate_locks`, `outbox`, `refunds`, `refund_payment_claims` | `SELECT`, `INSERT`, `UPDATE`, `DELETE` |

A migration adding a table that should not get the full operational grant must narrow it in the
same migration. `topup_app` also has `USAGE, SELECT` on `heartbeat_id_seq`. The database test
`application_role_privileges_match_the_documented_grants` checks every `public` table against this
table, so a new table fails it until it is listed here and in the test.

A repeated reconciliation block is ignored, and an update could rewrite a block's scope or chain,
so only the database owner can change a block. A `chain` block written by the address-derivation
check freezes that chain at runtime: pumps leave its deposits waiting, its scanner pauses, the
flusher plans nothing, and address issuance and rate-lock creation answer `423 chain_frozen`. The
service still starts and keeps serving other chains. An `address` block excludes one address from
flush planning after a credit recomputation mismatch. To lift a block after the cause has been
investigated and signed off, an operator calls the admin-signed
`POST /v1/admin/reconciliation-blocks/{block_key}/lift` with a `reason`: it deletes the row and
writes an `audit` row carrying the removed block in one transaction. The components resume on
their next iteration without a restart.

Points the schema does not show on its own:

- `products` stores no key id. The attested route's `destination.product_kid` is its only source
  (architecture §14); the service refuses to start when two loaded routes of one product disagree
  on it.
- `addresses.created_block` defaults to zero, which makes the first scanner pass check the full
  chain history before setting `backfilled`. The API and rate-lock paths set it from the chain's
  committed cursor instead.
- `accounts.closed_at` is informational. The product refuses a credit for a closed workspace by
  requesting its refund, not through a service-side closure check.
- `settlements` is the read-only record of the retired settlement protocol; `transitions` keeps
  the retired `cleared` state and `deposits.reason` the retired `product_refused` for history.
- `pending_transfers` is display-only, written by the head scan and cleared by the finalized
  scanner's cursor advance. Nothing that affects money reads it or `addresses.requested_at`.
- The heartbeat RPO target is the code constant `topup::heartbeat::RPO_SECONDS`, not a column.
