-- A sanctions hit on a credit delivered to the merchant before a restore is compliance, not
-- commercial policy: the delivered credit stands, and its forwarder is never offered for a sweep
-- (docs/design/payment-settings.md §11). The screen step records when the list named its sender.
ALTER TABLE deposits ADD COLUMN sanctions_hit_at timestamptz;

COMMENT ON COLUMN deposits.sanctions_hit_at IS
    'When screening named the sender of a deposit whose delivered credit stands; its forwarder is not swept.';
