-- Funds-direction and permission hardening before launch (docs/design/multi-tenant.md, "Launch
-- hardening" amendment).
--
-- Restricted keys (design PR 12, Stripe's restricted keys, https://docs.stripe.com/keys#limit-access)
-- are issued now. A restricted key holds the permissions granted to it (`api_keys.permissions`)
-- that the authorization table also grants to `key:restricted`; it never manages keys,
-- treasuries, webhook endpoints, webhook keys, or account settings, which stay with secret keys.
DELETE FROM permissions
WHERE principal = 'key:restricted'
  AND permission IN ('account.write', 'endpoints.write', 'treasury.write', 'api_keys.write');
ALTER TABLE api_keys ADD CONSTRAINT api_keys_permissions_array_check
    CHECK (permissions IS NULL OR jsonb_typeof(permissions) = 'array');
COMMENT ON COLUMN api_keys.permissions IS
    'A restricted key''s granted permission codes, such as ["quotes.write", "quotes.read"]; NULL for a secret key, which holds every permission.';

-- Per-treasury crediting pause: deposits to every forwarder over the treasury's address stay
-- pending, uncredited, until both the merchant's and the operator's pause are lifted. Neither lifts
-- the other, as with the account's `paused_scopes` and `self_paused_scopes`.
ALTER TABLE treasuries ADD COLUMN crediting_paused_by text[] NOT NULL DEFAULT '{}'
    CHECK (crediting_paused_by <@ ARRAY['merchant', 'operator']::text[]);
COMMENT ON COLUMN treasuries.crediting_paused_by IS
    'Who paused crediting of deposits to forwarders over this treasury address: merchant (POST /v1/treasuries/{id}/pause), operator (admin API). Empty when crediting runs.';

-- A webhook key roll's notice is signed by the version it retires as well, whenever it is
-- delivered, so a merchant that pinned only the old key still verifies the notice.
ALTER TABLE events ADD COLUMN signing_key_version integer CHECK (signing_key_version >= 1);
COMMENT ON COLUMN events.signing_key_version IS
    'A webhook key version that signs every delivery of this event beside the keys signing at delivery time, even after its overlap ended: the version a roll retired, on the roll''s account.updated.';
