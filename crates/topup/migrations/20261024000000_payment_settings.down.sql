-- Restores the schema before 20261024000000_payment_settings: the confirmation policies the
-- cutover kept come back, and a deposit rejected because its account did not accept its asset
-- becomes `unsupported_asset`, the reason the earlier schema has for a token it does not credit.
CREATE TABLE confirmation_policies (
    account_id uuid NOT NULL REFERENCES accounts(id),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    required text NOT NULL CHECK (
        required IN ('safe', 'finalized') OR required ~ '^[1-9][0-9]{0,5}$'
    ),
    PRIMARY KEY (account_id, chain_id)
);
INSERT INTO confirmation_policies (account_id, chain_id, required)
SELECT (policy ->> 'account_id')::uuid, (policy ->> 'chain_id')::bigint, policy ->> 'required'
FROM payment_settings_cutover, jsonb_array_elements(confirmation_policies) AS policy
WHERE (policy ->> 'account_id')::uuid IN (SELECT id FROM accounts);

DROP TABLE payment_settings_cutover;

DELETE FROM webhook_deliveries
WHERE event_id IN (SELECT id FROM events WHERE object_type = 'payment_settings');
DELETE FROM events WHERE object_type = 'payment_settings';
ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN (
        'deposit', 'quote', 'api_key', 'account', 'refund', 'treasury', 'webhook_endpoint'
    ));

UPDATE deposits SET reason = 'unsupported_asset' WHERE reason = 'asset_not_accepted';
ALTER TABLE deposits
    DROP CONSTRAINT deposits_reason_check,
    ADD CONSTRAINT deposits_reason_check CHECK (reason IN (
        'unsupported_asset', 'below_minimum', 'out_of_range', 'sanctioned', 'out_of_bounds'
    ));

ALTER TABLE quotes
    DROP CONSTRAINT quotes_terms_check,
    DROP CONSTRAINT quotes_settings_revision_fkey,
    DROP COLUMN terms,
    DROP COLUMN settings_revision_id,
    DROP COLUMN route_version;

DROP INDEX deposits_settings_hold_idx;
ALTER TABLE deposits
    DROP CONSTRAINT deposits_settings_binding_check,
    DROP CONSTRAINT deposits_settings_revision_fkey,
    DROP COLUMN settings_hold_id,
    DROP COLUMN settings_revision_id;

DROP TRIGGER accounts_payment_settings ON accounts;
DROP FUNCTION payment_settings_for_new_account();
DROP TABLE payment_settings_state;
DROP TABLE payment_settings_revisions;
