CREATE TABLE heartbeat (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    rpo_seconds integer NOT NULL DEFAULT 60 CHECK (rpo_seconds = 60)
);

GRANT SELECT, INSERT ON TABLE heartbeat TO topup_app;
GRANT USAGE, SELECT ON SEQUENCE heartbeat_id_seq TO topup_app;

COMMENT ON TABLE heartbeat IS
    'One row per minute; restore-check compares the restored row with an external failure point.';
