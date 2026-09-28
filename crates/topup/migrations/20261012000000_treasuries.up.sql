-- Treasuries through the API (docs/design/multi-tenant.md D10, §16 PR 7). An account sets one
-- treasury per chain and mode with an EIP-4361 proof (an EOA's signature, or a deployed contract's
-- EIP-1271 answer); the first one of a chain and every test-mode change apply at once, and a later
-- live change applies 48 hours after it is proven unless canceled first. Quotes and deposit
-- addresses take the chain's current treasury; forwarders already issued keep theirs for good.
--
-- 20261004000000 created `treasuries` without a mode or a lifecycle, and nothing wrote to it; the
-- migration refuses to run if a row exists.

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM treasuries) THEN
        RAISE EXCEPTION 'treasuries exist; they have no mode to migrate';
    END IF;
END;
$$;

DROP INDEX treasuries_account_chain_idx;
ALTER TABLE treasuries ADD COLUMN livemode boolean NOT NULL;
ALTER TABLE treasuries ADD COLUMN kind text NOT NULL CHECK (kind IN ('eoa', 'contract'));
ALTER TABLE treasuries ADD COLUMN applied_at timestamptz;
ALTER TABLE treasuries ADD COLUMN replaced_at timestamptz;
ALTER TABLE treasuries ADD COLUMN cancellation_reason text
    CHECK (cancellation_reason IN ('requested', 'sanctioned'));
ALTER TABLE treasuries ALTER COLUMN verified_at SET NOT NULL;
ALTER TABLE treasuries ALTER COLUMN effective_at SET NOT NULL;
ALTER TABLE treasuries ALTER COLUMN screened_at SET NOT NULL;
ALTER TABLE treasuries ALTER COLUMN created_by SET NOT NULL;
ALTER TABLE treasuries ADD CONSTRAINT treasuries_proof_signature_check
    CHECK (proof_signature ~ '^0x([0-9a-f]{2})*$');
ALTER TABLE treasuries ADD CONSTRAINT treasuries_lifecycle_check CHECK (
    (canceled_at IS NULL OR applied_at IS NULL) AND (replaced_at IS NULL OR applied_at IS NOT NULL)
    AND (canceled_at IS NULL) = (cancellation_reason IS NULL)
);
-- At most one change waits per chain, and one treasury is current per chain.
CREATE UNIQUE INDEX treasuries_pending_unique ON treasuries (account_id, livemode, chain_id)
    WHERE applied_at IS NULL AND canceled_at IS NULL;
CREATE UNIQUE INDEX treasuries_current_unique ON treasuries (account_id, livemode, chain_id)
    WHERE applied_at IS NOT NULL AND replaced_at IS NULL;
CREATE INDEX treasuries_due_idx ON treasuries (effective_at)
    WHERE applied_at IS NULL AND canceled_at IS NULL;
CREATE INDEX treasuries_screening_idx ON treasuries (screened_at)
    WHERE applied_at IS NOT NULL AND replaced_at IS NULL;
CREATE INDEX treasuries_scope_idx ON treasuries (account_id, livemode, created_at DESC, id DESC);

COMMENT ON TABLE treasuries IS
    'An account''s treasury of one chain and mode, proven with an EIP-4361 message: pending until effective_at, then current (applied_at) until replaced by the next (replaced_at), or canceled while pending.';
COMMENT ON COLUMN treasuries.kind IS
    'eoa: the message''s EIP-191 signature recovers to the address; contract: a contract deployed at the address returned the EIP-1271 magic value on both providers at finalized.';
COMMENT ON COLUMN treasuries.cancellation_reason IS
    'requested: the merchant canceled the pending change; sanctioned: a sanctions list named the treasury when it was due to apply.';
COMMENT ON COLUMN treasuries.screened_at IS
    'Last sanctions screening: when proven, when applied, and again daily while current.';
COMMENT ON COLUMN treasuries.effective_at IS
    'When the treasury applies: at once for the first treasury of a chain and in test mode, 48 hours after the proof for a later live change.';

-- Single-use EIP-4361 challenges, bound to the account, mode, chain, and address, valid 10 minutes.
CREATE TABLE treasury_challenges (
    nonce text PRIMARY KEY CHECK (nonce ~ '^[0-9a-f]{32}$'),
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    address text NOT NULL CHECK (
        address ~ '^0x[0-9a-f]{40}$' AND address <> '0x0000000000000000000000000000000000000000'
    ),
    message text NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX treasury_challenges_expires_idx ON treasury_challenges (expires_at);

-- A treasury change on a chain replaces that chain's network of every deposit address of the
-- account and mode.
CREATE INDEX addresses_deposit_address_chain_idx ON addresses (account_id, livemode, chain_id)
    WHERE deposit_address_id IS NOT NULL AND superseded_at IS NULL;

ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN ('deposit', 'quote', 'api_key', 'account', 'refund', 'treasury'));
