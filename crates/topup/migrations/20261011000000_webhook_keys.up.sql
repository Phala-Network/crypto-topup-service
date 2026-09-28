-- Design PR 6: per-account, per-mode webhook keys (docs/design/multi-tenant.md D9, D11).
--
-- `accounts.webhook_key_version` holds the current version of each mode's key, derived from
-- dstack KMS at `settlement/{account}/{live|test}/v{version}`; no secret is stored. A roll bumps
-- it and records the previous version here until the overlap ends: deliveries carry a signature
-- by every version listed here that has not expired, and attestation binds them too.

ALTER TABLE accounts DROP CONSTRAINT accounts_webhook_key_version_check;
ALTER TABLE accounts ADD CONSTRAINT accounts_webhook_key_version_check CHECK (
    (webhook_key_version ->> 'live') ~ '^[1-9][0-9]{0,8}$'
    AND (webhook_key_version ->> 'test') ~ '^[1-9][0-9]{0,8}$'
    AND jsonb_typeof(webhook_key_version -> 'live') = 'number'
    AND jsonb_typeof(webhook_key_version -> 'test') = 'number'
);

CREATE TABLE retiring_webhook_keys (
    account_id uuid NOT NULL REFERENCES accounts (id),
    livemode boolean NOT NULL,
    version integer NOT NULL CHECK (version >= 1),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (account_id, livemode, version)
);

COMMENT ON TABLE retiring_webhook_keys IS
    'Previous webhook key versions that still sign deliveries until expires_at (design D11).';
