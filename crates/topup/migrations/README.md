# Database migrations

Migrations run through the trusted database owner configured by `MIGRATE_DATABASE_URL`. The
`topup migrate` command never falls back to `DATABASE_URL`.

The service runs through the login role configured by `DATABASE_URL`. That login role must be a
member of the migration-created `topup_app` NOLOGIN role. `topup_app` can read and insert
`transitions` and `audit`, but cannot update, delete, or truncate them. Table owners and PostgreSQL
superusers remain trusted migration/operations identities. `BEFORE UPDATE OR DELETE` triggers stay
in place as defense in depth against accidental owner-side mutation.

Operational tables grant `SELECT`, `INSERT`, `UPDATE`, and `DELETE` to `topup_app`; no application
table grants `TRUNCATE`. Default privileges apply the same operational grant shape to future tables,
and migrations adding future append-only tables must narrow those grants in the same migration.

The scanner migration adds `addresses.created_block` and `addresses.backfilled`. Existing address
insertion APIs default `created_block` to zero, which is conservative: the first scanner pass checks
the full available chain history once before setting `backfilled` in the same transaction as the
observed deposits.

Migration `20260922000008_rate_lock_credit` refuses to run while a pre-existing rate lock remains
unconsumed because the frozen product credit cannot be reconstructed from the lock row without its
route decimal configuration. Before upgrading, stop quote issuance, let open locks expire or cancel
them in the product, reconcile that none received a payment, and remove those unconsumed rows. The
migration preserves already consumed rows with a non-operative zero sentinel; every lock created by
the upgraded service stores its exact frozen `credit_minor`.

Migration `20260922000014_refund_settlement_exclusion` persists the effective route on every refund
so later approval checks the same route pause selected at request time. Before upgrading an
environment with pre-existing refunds whose deposits have no route, reconcile and populate an
effective fallback route; the migration refuses to invent one.

Migration `20260922000017_reconciliation` adds the append-only `reconciliation_findings` table,
the `reconciliation_blocks` freeze table, and the reconciler's incremental scan cursors.
`topup_app` can read and insert findings but cannot update, delete, or truncate them. It can insert
and update cursors but cannot delete them. Migration `20260922000019_reconciliation_blocks_insert_only`
narrows blocks to read and insert: a repeated block is ignored, and an update could rewrite a block's
scope or chain, so only the database owner can change or lift a block.

A `chain` block written by the address-derivation check freezes that chain at runtime: pumps leave
its deposits waiting, its scanner pauses, the flusher plans nothing, and address issuance and
rate-lock creation answer `423 chain_frozen`. The service still starts and keeps serving other
chains. An `address` block excludes one address from flush planning after a credit recomputation
mismatch. To lift a block after the cause has been investigated and signed off, the owner deletes
the row; the components resume on their next iteration without a restart:

```sql
DELETE FROM reconciliation_blocks WHERE block_key = 'chain:<chain_id>';
DELETE FROM reconciliation_blocks WHERE block_key = 'address:<address_id>';
```

Migration `20260922000021_route_settlement_destination` drops `products.settlement_url` and
`products.kid`. The attested route's `destination.settlement_url` and `destination.product_kid`
are the only source of both values (architecture §14); the service refuses to start when two loaded
routes of one product disagree on either. Its down migration restores the columns empty; copy the
values back from the route files before running an older service.

Dropping these columns is a deliberate exception to the additive-migrations rule (plan §6 item 6):
no environment has been deployed, and a later work package squashes all migrations into one
initial schema. Do not treat it as precedent once any environment holds data.

Migration `20260922000022_pending_transfers` adds the display-only `pending_transfers` table
(`SELECT`, `INSERT`, `UPDATE`, `DELETE` for `topup_app`) written by the head scan and cleared by the
finalized scanner's cursor advance, and `addresses.requested_at`, the last time the product issued
or fetched a persistent address. Existing addresses get the migration time. Nothing that affects
money reads either.
