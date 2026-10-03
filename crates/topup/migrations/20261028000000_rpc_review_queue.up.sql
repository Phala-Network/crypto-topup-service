-- no-transaction
-- topup migrate verifies existing objects and repairs matching invalid builds before this statement.
CREATE INDEX CONCURRENTLY IF NOT EXISTS rpc_window_reviews_due_idx
ON public.rpc_window_reviews (chain_id, epoch, COALESCE(replayed_at, created_at), from_block, id)
WHERE reviewed_at IS NULL;
