-- An address's one-time backfill (its creation block through the chain's cursor, architecture §8)
-- keeps its progress window by window, so a failed pass, a restart, or a provider refusal resumes it
-- instead of reading the whole range again: a backfill longer than one provider budget otherwise
-- never completes, and the chain's finalized cursor waits on it.
ALTER TABLE addresses
    ADD COLUMN backfilled_through bigint CHECK (backfilled_through >= 0);

COMMENT ON COLUMN addresses.backfilled_through IS
    'Last block through which the scanner committed this address''s backfill before marking it backfilled; null before its first window. The backfill resumes after it.';
