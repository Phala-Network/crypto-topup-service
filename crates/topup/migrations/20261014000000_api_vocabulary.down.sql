-- Restores the schema before 20261014000000_api_vocabulary. A merchant's own quotes pause is
-- carried into the account's paused_scopes so it is not lost.

DELETE FROM permissions WHERE permission = 'forwarders.read';
DROP INDEX flushed_created_idx;
DROP INDEX flushed_id_unique;
ALTER TABLE flushed DROP COLUMN id;
DROP TABLE deposit_address_client_secrets;
UPDATE accounts
SET paused_scopes = ARRAY(SELECT DISTINCT unnest(paused_scopes || self_paused_scopes) ORDER BY 1)
WHERE self_paused_scopes <> '{}';
ALTER TABLE accounts DROP COLUMN self_paused_scopes;
