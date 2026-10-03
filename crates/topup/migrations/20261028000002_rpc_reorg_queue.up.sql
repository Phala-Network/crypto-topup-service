-- no-transaction
-- topup migrate verifies existing objects and repairs matching invalid builds before this statement.
CREATE INDEX CONCURRENTLY IF NOT EXISTS rpc_reorg_ranges_pending_idx
ON public.rpc_reorg_ranges (chain_id, epoch, COALESCE(replayed_through + 1, from_block))
WHERE COALESCE(replayed_through, from_block - 1) < to_block;
