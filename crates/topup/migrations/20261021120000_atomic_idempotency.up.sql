-- Atomic idempotent requests (architecture §12) and the permissions of API key kinds in code
-- (design D13).
--
-- A request claims its idempotency key under a fresh owner. Its changes and its saved response
-- commit in one transaction, which locks the key's row and checks the request still owns it, so a
-- repeat that takes over a key whose request never saved a response fences that request out.
-- A row without an owner gets one no request holds, as do the rows claimed before this migration.
ALTER TABLE idempotency_keys ADD COLUMN owner uuid NOT NULL DEFAULT gen_random_uuid();

COMMENT ON COLUMN idempotency_keys.owner IS
    'The request that holds the key, a fresh id per claim; a takeover replaces it, and only the owner saves a response.';
COMMENT ON COLUMN idempotency_keys.response IS
    'The first response, {"status", "body"}, replayed to repeats, saved in the transaction of the request''s changes; NULL while that request runs. An API key''s secret is never stored.';

-- A secret key holds every permission and a restricted key all but four, which only migrations
-- could change; `crate::tenancy::Principal` holds them now, beside the routes that require them.
DROP TABLE permissions;
