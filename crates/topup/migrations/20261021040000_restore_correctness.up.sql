-- Restore correctness (docs/architecture.md §14, docs/design/multi-tenant.md §13).

-- The credit of each deposit a merchant was told about in a delivered `deposit.credited` or
-- `deposit.reversed` that the service signed, imported after a restore. A settled amount is
-- immutable, so the confirm step values the deposit re-derived from the chain at it instead of at
-- spot, and its refunds and reversal reference it. A deposit whose transfer on chain contradicts
-- it is held, not valued or credited, until the operator discards the delivered credit.
CREATE TABLE restore_delivered_credits (
    deposit_id uuid PRIMARY KEY,
    event_id uuid NOT NULL REFERENCES restore_delivered_events(event_id),
    restore_id uuid NOT NULL REFERENCES restores(id),
    account_id uuid NOT NULL,
    livemode boolean NOT NULL,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    address text NOT NULL CHECK (address ~ '^0x[0-9a-f]{40}$'),
    asset_contract text NOT NULL CHECK (asset_contract ~ '^0x[0-9a-f]{40}$'),
    from_address text NOT NULL CHECK (from_address ~ '^0x[0-9a-f]{40}$'),
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    price_scaled numeric(78,0) NOT NULL CHECK (price_scaled >= 0),
    price_source text NOT NULL CHECK (price_source IN ('spot', 'lock')),
    credit_minor numeric(78,0) NOT NULL CHECK (credit_minor >= 0),
    valuation_at timestamptz NOT NULL,
    discarded_at timestamptz,
    discarded_by text,
    discard_reason text,
    CONSTRAINT restore_delivered_credits_discard_complete CHECK (
        (discarded_at IS NULL AND discarded_by IS NULL AND discard_reason IS NULL)
        OR (discarded_at IS NOT NULL AND discarded_by IS NOT NULL AND discard_reason IS NOT NULL)
    )
);
CREATE INDEX restore_delivered_credits_restore_idx ON restore_delivered_credits (restore_id);
REVOKE DELETE ON TABLE restore_delivered_credits FROM topup_app;
COMMENT ON COLUMN restore_delivered_credits.discarded_at IS
    'When the operator discarded a delivered credit the chain contradicts; the deposit is then valued as any other.';

-- A quote re-issued after a restore from the merchant's record of it. Its locked terms are the
-- merchant's record, not the service's, so the confirm step never values a payment at them: a
-- payment to it is valued at a delivered credit (restore_delivered_credits) or at spot.
ALTER TABLE quotes ADD COLUMN restore_id uuid REFERENCES restores(id);
COMMENT ON COLUMN quotes.restore_id IS
    'The restore that re-issued the quote from the merchant''s record; its locked price is never applied.';
