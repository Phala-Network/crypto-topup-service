-- Rate-lock creation checks each cap against the sum of open reserved locks under an advisory
-- lock, so the per-scope counters are no longer kept.
DROP TABLE lock_exposure;
