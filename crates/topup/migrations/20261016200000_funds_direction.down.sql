-- Restores the schema before 20261016200000_funds_direction. Restricted keys are revoked, as the
-- earlier schema never issued them.

ALTER TABLE events DROP COLUMN signing_key_version;
ALTER TABLE treasuries DROP COLUMN crediting_paused_by;
UPDATE api_keys SET revoked_at = now() WHERE kind = 'restricted' AND revoked_at IS NULL;
COMMENT ON COLUMN api_keys.permissions IS NULL;
ALTER TABLE api_keys DROP CONSTRAINT api_keys_permissions_array_check;
INSERT INTO permissions (permission, principal)
VALUES ('account.write', 'key:restricted'), ('endpoints.write', 'key:restricted')
ON CONFLICT DO NOTHING;
