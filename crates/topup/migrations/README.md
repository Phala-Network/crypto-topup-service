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
the owner creates; no application table grants `TRUNCATE`. The initial schema narrows that grant:

| Tables | `topup_app` |
|---|---|
| `transitions`, `audit`, `reconciliation_findings`, `heartbeat` | `SELECT`, `INSERT` (append-only) |
| `reconciliation_blocks` | `SELECT`, `INSERT` |
| `reconciliation_deposit_cursors`, `reconciliation_custody_cursors` | `SELECT`, `INSERT`, `UPDATE` |
| `_sqlx_migrations` | `SELECT` |
| every other table | `SELECT`, `INSERT`, `UPDATE`, `DELETE` |

A migration adding a table that should not get the full operational grant must narrow it in the
same migration. `topup_app` also has `USAGE, SELECT` on `heartbeat_id_seq`.

A repeated reconciliation block is ignored, and an update could rewrite a block's scope or chain,
so only the database owner can change or lift a block. A `chain` block written by the
address-derivation check freezes that chain at runtime: pumps leave its deposits waiting, its
scanner pauses, the flusher plans nothing, and address issuance and rate-lock creation answer
`423 chain_frozen`. The service still starts and keeps serving other chains. An `address` block
excludes one address from flush planning after a credit recomputation mismatch. To lift a block
after the cause has been investigated and signed off, the owner deletes the row; the components
resume on their next iteration without a restart:

```sql
DELETE FROM reconciliation_blocks WHERE block_key = 'chain:<chain_id>';
DELETE FROM reconciliation_blocks WHERE block_key = 'address:<address_id>';
```

Points the schema does not show on its own:

- `products` stores no settlement URL or key id. The attested route's `destination.settlement_url`
  and `destination.product_kid` are the only source of both values (architecture §14); the service
  refuses to start when two loaded routes of one product disagree on either.
- `addresses.created_block` defaults to zero, which makes the first scanner pass check the full
  chain history before setting `backfilled`. The API and rate-lock paths set it from the chain's
  committed cursor instead.
- `accounts.closed_at` is informational. Late funds are refundable because the product answers
  `rejected` (recorded as `product_refused`), not through a service-side closure check.
- `pending_transfers` is display-only, written by the head scan and cleared by the finalized
  scanner's cursor advance. Nothing that affects money reads it or `addresses.requested_at`.
- The heartbeat RPO target is the code constant `topup::heartbeat::RPO_SECONDS`, not a column.
