ALTER TABLE outbox
    ADD COLUMN attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();

COMMENT ON COLUMN outbox.attempts IS
    'Number of failed delivery attempts; successful delivery does not increment this counter.';
COMMENT ON COLUMN outbox.created_at IS
    'Stable event creation time used in the webhook envelope and age alerts.';
