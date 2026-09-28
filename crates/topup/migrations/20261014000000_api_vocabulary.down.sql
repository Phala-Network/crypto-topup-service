-- Restores the schema before 20261014000000_api_vocabulary. A merchant's own quotes pause is
-- carried into the account's paused_scopes so it is not lost.

DELETE FROM permissions WHERE permission = 'addresses.read';
DROP TABLE deposit_address_client_secrets;
UPDATE accounts
SET paused_scopes = ARRAY(SELECT DISTINCT unnest(paused_scopes || self_paused_scopes) ORDER BY 1)
WHERE self_paused_scopes <> '{}';
ALTER TABLE accounts DROP COLUMN self_paused_scopes;
