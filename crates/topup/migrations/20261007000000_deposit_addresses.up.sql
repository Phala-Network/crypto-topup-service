-- Deposit addresses (docs/design/multi-tenant.md "Deposit addresses"): a customer's persistent,
-- rotatable forwarder address per chain and asset, restored per the owner's 2026-09-21
-- requirement. Payments to it are credited at spot through the same pipeline as quote payments;
-- the forwarder itself is an ordinary `addresses` row, now owned by a quote or a deposit address.

CREATE TABLE deposit_addresses (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    customer_id uuid NOT NULL,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    asset text NOT NULL CHECK (char_length(asset) BETWEEN 1 AND 64),
    route text NOT NULL,
    version bigint NOT NULL CHECK (version >= 1),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'retired')),
    created_at timestamptz NOT NULL DEFAULT now(),
    retired_at timestamptz,
    metadata jsonb NOT NULL DEFAULT '{}'
        CONSTRAINT deposit_addresses_metadata_check CHECK (metadata_is_valid(metadata)),
    CONSTRAINT deposit_addresses_scope_unique UNIQUE (id, account_id, livemode),
    CONSTRAINT deposit_addresses_version_unique UNIQUE (customer_id, chain_id, asset, version),
    CONSTRAINT deposit_addresses_customer_fkey FOREIGN KEY (customer_id, account_id, livemode)
        REFERENCES customers (id, account_id, livemode),
    CONSTRAINT deposit_addresses_retired_check
        CHECK ((status = 'active') = (retired_at IS NULL))
);

-- At most one active address per customer, chain, and asset: creation returns it.
CREATE UNIQUE INDEX deposit_addresses_active_unique
    ON deposit_addresses (customer_id, chain_id, asset)
    WHERE status = 'active';
-- Merchant listings read one account and mode, newest first; the cap counts its active rows.
CREATE INDEX deposit_addresses_scope_created_idx
    ON deposit_addresses (account_id, livemode, created_at, id);
CREATE INDEX deposit_addresses_active_scope_idx
    ON deposit_addresses (account_id, livemode)
    WHERE status = 'active';
-- The rotation rate limit counts a customer's recent retirements.
CREATE INDEX deposit_addresses_customer_retired_idx
    ON deposit_addresses (customer_id, retired_at)
    WHERE status = 'retired';

COMMENT ON TABLE deposit_addresses IS
    'A customer''s persistent deposit address for one chain and asset; retired by rotation, and still credited at spot when retired.';
COMMENT ON COLUMN deposit_addresses.asset IS
    'The route''s asset code; with the account, mode, client_reference_id, chain, and version it derives the salt.';
COMMENT ON COLUMN deposit_addresses.route IS
    'The route the address was issued on; its current version supplies the token contract of payment_uri.';
COMMENT ON COLUMN deposit_addresses.metadata IS
    'The merchant''s key/value pairs; set on create, merged by POST /v1/deposit_addresses/{id}, carried to the next version on rotation, and copied to each deposit when it is recorded.';
COMMENT ON COLUMN deposit_addresses.version IS
    'Counts the customer''s addresses for the chain and asset from 1; each rotation takes the next.';

-- The forwarder row belongs to exactly one quote or one deposit address, of the same account and
-- mode.
ALTER TABLE addresses ALTER COLUMN quote_id DROP NOT NULL;
ALTER TABLE addresses ADD COLUMN deposit_address_id uuid;
ALTER TABLE addresses ADD CONSTRAINT addresses_deposit_address_unique UNIQUE (deposit_address_id);
ALTER TABLE addresses ADD CONSTRAINT addresses_deposit_address_fkey
    FOREIGN KEY (deposit_address_id, account_id, livemode)
    REFERENCES deposit_addresses (id, account_id, livemode);
ALTER TABLE addresses ADD CONSTRAINT addresses_owner_check
    CHECK (num_nonnulls(quote_id, deposit_address_id) = 1);

COMMENT ON COLUMN addresses.deposit_address_id IS
    'The deposit address this forwarder is; null for a quote''s address.';

-- Per-account override of the default cap on active deposit addresses (design §12); null keeps
-- the service default of the mode.
ALTER TABLE account_limits ADD COLUMN max_active_deposit_addresses integer
    CHECK (max_active_deposit_addresses > 0);

INSERT INTO permissions (permission, principal)
VALUES
    ('deposit_addresses.read', 'key:secret'),
    ('deposit_addresses.write', 'key:secret'),
    ('deposit_addresses.read', 'key:restricted'),
    ('deposit_addresses.write', 'key:restricted');

COMMENT ON COLUMN deposits.metadata IS
    'The merchant''s key/value pairs, a copy of the quote''s or the deposit address''s when the deposit is recorded and independent after; updated by POST /v1/deposits/{id}.';
