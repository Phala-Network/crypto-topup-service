-- Operator onboarding and API keys (docs/design/multi-tenant.md D7, D8, D12, §14, §16 PR 5).
--
-- The 2026-09-28 amendment removes the merchant dashboard: the operator creates every account
-- through the admin API after offline due diligence, and merchants act only through API keys.
-- This drops what 20261004000000_multi_tenant created for the dashboard (users and their logins,
-- memberships, sessions, role grants, the self-serve activation fields) and the RFC 9421 merchant
-- request keys, and adds the account's contact and due diligence record and each event's actor.

DROP TABLE request_signing_keys;

-- People: there are none. API keys and treasuries name the API key (or `admin`) that made them.
ALTER TABLE api_keys DROP COLUMN created_by;
ALTER TABLE api_keys ADD COLUMN created_by text NOT NULL DEFAULT 'admin'
    CHECK (created_by = 'admin' OR created_by ~ '^key_[0-9a-f]{32}$');
ALTER TABLE api_keys ALTER COLUMN created_by DROP DEFAULT;
ALTER TABLE treasuries DROP COLUMN created_by;
ALTER TABLE treasuries ADD COLUMN created_by text CHECK (created_by ~ '^key_[0-9a-f]{32}$');

DROP TABLE sessions;
DROP TABLE invitations;
DROP TABLE memberships;
DROP TABLE recovery_codes;
DROP TABLE passkeys;
DROP TABLE identities;
DROP TABLE users;

ALTER TABLE audit DROP CONSTRAINT audit_actor_type_check;
ALTER TABLE audit ADD CONSTRAINT audit_actor_type_check
    CHECK (actor_type IN ('api_key', 'admin', 'system'));

-- Accounts: the operator decides live mode (`charges_enabled`) on its offline due diligence, of
-- which only a reference, the date, and the reviewer are kept; `contact` (name, security email)
-- is the only personal data kept about a merchant.
ALTER TABLE accounts DROP CONSTRAINT accounts_charges_enabled_check;
ALTER TABLE accounts DROP COLUMN business_profile;
ALTER TABLE accounts DROP COLUMN country;
ALTER TABLE accounts DROP COLUMN tos_acceptance;
ALTER TABLE accounts DROP COLUMN live_access;
ALTER TABLE accounts ADD COLUMN contact jsonb NOT NULL DEFAULT '{}'
    CHECK (jsonb_typeof(contact) = 'object');
ALTER TABLE accounts ADD COLUMN due_diligence jsonb NOT NULL DEFAULT '{}'
    CHECK (jsonb_typeof(due_diligence) = 'object');

COMMENT ON COLUMN accounts.contact IS
    'The merchant''s contact, {"name", "email"}: the operator''s channel for key hand-over, recovery, incidents, and restores.';
COMMENT ON COLUMN accounts.due_diligence IS
    'The offline due diligence record, {"reference", "reviewed_at", "reviewed_by"}.';
COMMENT ON COLUMN accounts.charges_enabled IS
    'Live mode, set by the operator; live keys answer 403 testmode_charges_only while false.';

-- The authorization table holds API key kinds only. A secret key holds every permission; a
-- restricted key (design PR 12) may be granted the API permissions except `api_keys.write` and
-- `treasury.write`.
DELETE FROM permissions
WHERE principal NOT IN ('key:secret', 'key:restricted')
   OR permission IN (
       'keys.read', 'keys.write', 'members.read', 'members.write', 'audit.read',
       'activation.write', 'ownership.write'
   );
ALTER TABLE permissions DROP CONSTRAINT permissions_principal_check;
ALTER TABLE permissions ADD CONSTRAINT permissions_principal_check
    CHECK (principal IN ('key:secret', 'key:restricted'));
INSERT INTO permissions (permission, principal)
VALUES
    ('api_keys.read', 'key:secret'),
    ('api_keys.write', 'key:secret'),
    ('treasury.read', 'key:secret'),
    ('treasury.write', 'key:secret'),
    ('account.write', 'key:secret'),
    ('api_keys.read', 'key:restricted'),
    ('treasury.read', 'key:restricted'),
    ('account.write', 'key:restricted');

-- Events: key and account changes are events too, and every event names who caused it: an API
-- key id, `admin`, or `system`.
ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN ('deposit', 'quote', 'api_key', 'account'));
ALTER TABLE events ADD COLUMN actor text NOT NULL DEFAULT 'system'
    CHECK (actor IN ('admin', 'system') OR actor ~ '^key_[0-9a-f]{32}$');
ALTER TABLE events ALTER COLUMN actor DROP DEFAULT;

-- `idempotency_keys` serves every merchant POST, so the per-object keys go.
DROP INDEX quotes_idempotency_key_unique;
ALTER TABLE quotes DROP COLUMN idempotency_key;
DROP INDEX refunds_idempotency_key_unique;
ALTER TABLE refunds DROP COLUMN idempotency_key;

COMMENT ON COLUMN quotes.client_secret_hash IS
    'SHA-256 of the quote''s client_secret, the bearer of its public read.';

-- A stored response is {"status": <HTTP status>, "body": <JSON body>}; NULL while the first
-- request with the key still runs. Rows older than 24 hours are pruned.
CREATE INDEX idempotency_keys_created_idx ON idempotency_keys (created_at);

COMMENT ON COLUMN idempotency_keys.fingerprint IS
    'SHA-256 of the request method, path and query, and body; a repeat with another fingerprint is refused.';
COMMENT ON COLUMN idempotency_keys.response IS
    'The first response, {"status", "body"}, replayed to repeats; NULL while that request runs. An API key''s secret is never stored.';

COMMENT ON COLUMN api_keys.key_hash IS
    'SHA-256 of the whole key; the key itself is shown once and never stored.';
COMMENT ON COLUMN api_keys.expires_at IS
    'Set when the key is rolled: the old key keeps working until then, at most 7 days.';
