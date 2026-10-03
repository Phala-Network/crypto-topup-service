-- no-transaction
CREATE INDEX CONCURRENTLY rpc_window_reviews_pending
ON public.rpc_window_reviews (chain_id, from_block) WHERE reviewed_at IS NULL;
