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
