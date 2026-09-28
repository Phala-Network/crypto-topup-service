-- Restore mode (docs/architecture.md §14, docs/design/multi-tenant.md §13): a database restored
-- from backup starts frozen until the operator has reconciled it and unfreezes it.

-- The PostgreSQL timeline the service last acknowledged. Every promotion out of archive recovery
-- (a restore from backup) starts a new timeline, and crash recovery does not, so `topup run`
-- finds a restore that booted straight into the service compose by comparing the current timeline
-- with this one. The first value is the timeline this migration runs on.
CREATE TABLE restore_timeline (
    singleton boolean PRIMARY KEY DEFAULT true CONSTRAINT restore_timeline_singleton CHECK (singleton),
    timeline_id bigint NOT NULL CHECK (timeline_id > 0)
);
INSERT INTO restore_timeline (timeline_id)
VALUES (('x' || left(pg_walfile_name(pg_current_wal_insert_lsn()), 8))::bit(32)::bigint);
REVOKE INSERT, DELETE ON TABLE restore_timeline FROM topup_app;
COMMENT ON TABLE restore_timeline IS
    'The PostgreSQL timeline the service last acknowledged; a newer one means the database was restored from backup.';

-- One row per detected restore. The row without `unfrozen_at` is the freeze: merchant writes
-- answer 503 service_restoring and nothing credits, settles, applies a treasury change, or
-- delivers an event until the operator unfreezes it through the admin API (audited).
CREATE TABLE restores (
    id uuid PRIMARY KEY,
    detected_at timestamptz NOT NULL DEFAULT now(),
    detected_by text NOT NULL CHECK (detected_by IN ('restore_check', 'timeline')),
    timeline_id bigint NOT NULL CHECK (timeline_id > 0),
    restore_point timestamptz,
    restored_cursors jsonb NOT NULL DEFAULT '{}'
        CONSTRAINT restores_restored_cursors_object CHECK (jsonb_typeof(restored_cursors) = 'object'),
    unfrozen_at timestamptz,
    unfrozen_by text,
    unfreeze_reason text,
    CONSTRAINT restores_unfreeze_complete CHECK (
        (unfrozen_at IS NULL AND unfrozen_by IS NULL AND unfreeze_reason IS NULL)
        OR (unfrozen_at IS NOT NULL AND unfrozen_by IS NOT NULL AND unfreeze_reason IS NOT NULL)
    )
);
CREATE UNIQUE INDEX restores_one_frozen ON restores ((true)) WHERE unfrozen_at IS NULL;
REVOKE DELETE ON TABLE restores FROM topup_app;
COMMENT ON COLUMN restores.restore_point IS
    'Newest heartbeat in the restored database: changes after it may be lost.';
COMMENT ON COLUMN restores.restored_cursors IS
    'Each chain''s scanned_block when the restore was detected; the rescan starts there, and a re-issued address is backfilled from it.';

-- Events the merchant received after the restore point, imported from the merchant's records as
-- delivered: the stored snapshot is the truth, so a deposit re-derived by the rescan never
-- re-emits its event with another body. Imported events get no delivery.
CREATE TABLE restore_delivered_events (
    event_id uuid PRIMARY KEY REFERENCES events(id),
    restore_id uuid NOT NULL REFERENCES restores(id),
    imported_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX restore_delivered_events_restore_idx ON restore_delivered_events (restore_id);
REVOKE UPDATE, DELETE ON TABLE restore_delivered_events FROM topup_app;
