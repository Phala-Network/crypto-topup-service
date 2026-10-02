-- Per-account payment settings (docs/design/payment-settings.md). Each account and mode has one
-- state row naming its current revision; revisions are immutable and append-only. An account
-- created later gets an `unconfigured` revision per mode from the trigger below, so every account
-- and mode always has a state and accepts nothing until it is configured.
CREATE TABLE payment_settings_revisions (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    kind text NOT NULL CHECK (kind IN ('unconfigured', 'configured', 'legacy')),
    document jsonb NOT NULL CHECK (jsonb_typeof(document) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    created_by text NOT NULL,
    CONSTRAINT payment_settings_revisions_scope_unique UNIQUE (id, account_id, livemode)
);

COMMENT ON TABLE payment_settings_revisions IS
    'Immutable payment settings revisions; ids are random and never reused. `legacy`: the 0.5.0 model, which the cutover bound existing deposits and quotes to; never current.';

-- `held`: after a restore, until the merchant reconfirms with POST /v1/payment_settings; deposits
-- recorded meanwhile name the restore instead of a revision.
CREATE TABLE payment_settings_state (
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    status text NOT NULL CHECK (status IN ('unconfigured', 'configured', 'held')),
    current_revision_id uuid NOT NULL,
    held_by uuid REFERENCES restores(id),
    PRIMARY KEY (account_id, livemode),
    CONSTRAINT payment_settings_state_revision_fkey FOREIGN KEY (current_revision_id, account_id, livemode)
        REFERENCES payment_settings_revisions (id, account_id, livemode),
    CONSTRAINT payment_settings_state_hold_check CHECK ((status = 'held') = (held_by IS NOT NULL))
);

INSERT INTO payment_settings_revisions (id, account_id, livemode, kind, document, created_by)
SELECT gen_random_uuid(), account.id, mode.livemode, 'unconfigured', '{"chains": []}', 'system'
FROM accounts AS account CROSS JOIN (VALUES (false), (true)) AS mode (livemode);
INSERT INTO payment_settings_state (account_id, livemode, status, current_revision_id)
SELECT account_id, livemode, 'unconfigured', id FROM payment_settings_revisions;

CREATE FUNCTION payment_settings_for_new_account() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    mode boolean;
    revision uuid;
BEGIN
    FOREACH mode IN ARRAY ARRAY[false, true] LOOP
        revision := gen_random_uuid();
        INSERT INTO payment_settings_revisions (id, account_id, livemode, kind, document, created_by)
        VALUES (revision, NEW.id, mode, 'unconfigured', '{"chains": []}', 'system');
        INSERT INTO payment_settings_state (account_id, livemode, status, current_revision_id)
        VALUES (NEW.id, mode, 'unconfigured', revision);
    END LOOP;
    RETURN NULL;
END
$$;

CREATE TRIGGER accounts_payment_settings AFTER INSERT ON accounts
    FOR EACH ROW EXECUTE FUNCTION payment_settings_for_new_account();

-- Revisions are append-only for the service.
REVOKE UPDATE, DELETE ON payment_settings_revisions FROM topup_app;

-- A deposit is bound to exactly one of a revision of its own account and mode, or the restore
-- whose hold it was recorded under (design §7). The check is validated by the cutover backfill,
-- which binds every deposit recorded before this migration.
ALTER TABLE deposits
    ADD COLUMN settings_revision_id uuid,
    ADD COLUMN settings_hold_id uuid REFERENCES restores(id),
    ADD CONSTRAINT deposits_settings_revision_fkey
        FOREIGN KEY (settings_revision_id, account_id, livemode)
        REFERENCES payment_settings_revisions (id, account_id, livemode),
    ADD CONSTRAINT deposits_settings_binding_check
        CHECK ((settings_revision_id IS NULL) <> (settings_hold_id IS NULL)) NOT VALID;

CREATE INDEX deposits_settings_hold_idx ON deposits (account_id, livemode)
    WHERE settings_hold_id IS NOT NULL;

-- A quote keeps the terms it was issued with: its route version, the revision they were resolved
-- from (none for a quote re-issued after a restore, whose terms are never applied), and the
-- resolved terms. Validated by the cutover backfill, which gives every earlier quote its terms.
ALTER TABLE quotes
    ADD COLUMN route_version bigint CHECK (route_version >= 0),
    ADD COLUMN settings_revision_id uuid,
    ADD COLUMN terms jsonb,
    ADD CONSTRAINT quotes_settings_revision_fkey
        FOREIGN KEY (settings_revision_id, account_id, livemode)
        REFERENCES payment_settings_revisions (id, account_id, livemode),
    ADD CONSTRAINT quotes_terms_check CHECK (
        route_version IS NOT NULL AND jsonb_typeof(terms) = 'object'
        AND (settings_revision_id IS NOT NULL OR restore_id IS NOT NULL)
    ) NOT VALID;

ALTER TABLE deposits
    DROP CONSTRAINT deposits_reason_check,
    ADD CONSTRAINT deposits_reason_check CHECK (reason IN (
        'unsupported_asset', 'below_minimum', 'out_of_range', 'sanctioned', 'out_of_bounds',
        'asset_not_accepted'
    ));

ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN (
        'deposit', 'quote', 'api_key', 'account', 'refund', 'treasury', 'webhook_endpoint',
        'payment_settings'
    ));

-- The 0.6.0 cutover (design §10). An instance with issued addresses has deposits or quotes to
-- bind: `topup migrate --config` runs the backfill (`topup::payment_config::backfill`), which reads
-- the confirmation policies kept here, and recording stays held until the operator resumes it
-- with POST /v1/admin/recording/resume once the accounts are configured. A new instance has
-- nothing to bind, so its cutover is complete at once.
CREATE TABLE payment_settings_cutover (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    confirmation_policies jsonb NOT NULL,
    backfilled_at timestamptz,
    recording_resumed_at timestamptz,
    resumed_by text,
    resume_reason text,
    CONSTRAINT payment_settings_cutover_order_check
        CHECK (recording_resumed_at IS NULL OR backfilled_at IS NOT NULL)
);

INSERT INTO payment_settings_cutover (confirmation_policies, backfilled_at, recording_resumed_at)
SELECT COALESCE(
           (SELECT jsonb_agg(jsonb_build_object(
                       'account_id', account_id, 'chain_id', chain_id, 'required', required))
            FROM confirmation_policies),
           '[]'::jsonb
       ),
       CASE WHEN issued THEN NULL ELSE now() END,
       CASE WHEN issued THEN NULL ELSE now() END
FROM (SELECT EXISTS (SELECT 1 FROM addresses) AS issued) AS instance;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM addresses) THEN
        ALTER TABLE deposits VALIDATE CONSTRAINT deposits_settings_binding_check;
        ALTER TABLE quotes VALIDATE CONSTRAINT quotes_terms_check;
    END IF;
END
$$;

REVOKE INSERT, DELETE ON payment_settings_cutover FROM topup_app;

DROP TABLE confirmation_policies;
