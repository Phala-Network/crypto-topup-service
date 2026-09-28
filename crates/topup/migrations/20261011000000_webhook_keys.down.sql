-- Restores the schema before 20261011000000_webhook_keys.

DROP TABLE retiring_webhook_keys;
ALTER TABLE accounts DROP CONSTRAINT accounts_webhook_key_version_check;
ALTER TABLE accounts ADD CONSTRAINT accounts_webhook_key_version_check CHECK (
    jsonb_typeof(webhook_key_version -> 'live') = 'number'
    AND jsonb_typeof(webhook_key_version -> 'test') = 'number'
);
