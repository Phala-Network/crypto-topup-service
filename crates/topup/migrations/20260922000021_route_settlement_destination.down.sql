-- The dropped values live in the attested route files; restore them from there after a rollback.
ALTER TABLE products
    ADD COLUMN settlement_url text NOT NULL DEFAULT '',
    ADD COLUMN kid text NOT NULL DEFAULT '';
ALTER TABLE products
    ALTER COLUMN settlement_url DROP DEFAULT,
    ALTER COLUMN kid DROP DEFAULT;
