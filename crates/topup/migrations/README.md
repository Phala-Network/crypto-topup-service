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
