-- Restore closure (docs/architecture.md §14, docs/design/multi-tenant.md §13).

-- A deposit reversed after the restore point and lost with it, restored from the delivered
-- `deposit.reversed` the service signed: the reversed deposit's row takes back its revision at its
-- receipt position, so the rescan records the transfer now at the position under the next
-- revision, and `successor_id`, the event's `replaced_by`, names the deposit that replaces it once
-- it is recorded. The merchant's deposits keep the ids, links, and credits it was sent.
CREATE TABLE restore_deposit_tombstones (
    deposit_id uuid PRIMARY KEY REFERENCES deposits(id),
    event_id uuid NOT NULL REFERENCES events(id),
    restore_id uuid NOT NULL REFERENCES restores(id),
    successor_id uuid UNIQUE
);
CREATE INDEX restore_deposit_tombstones_restore_idx ON restore_deposit_tombstones (restore_id);
REVOKE UPDATE, DELETE ON TABLE restore_deposit_tombstones FROM topup_app;
COMMENT ON COLUMN restore_deposit_tombstones.successor_id IS
    'The deposit the delivered deposit.reversed named in replaced_by: recorded by the rescan, it replaces this one.';
