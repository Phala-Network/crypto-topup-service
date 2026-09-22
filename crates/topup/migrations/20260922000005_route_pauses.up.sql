ALTER TABLE products
    ADD CONSTRAINT products_slug_unique UNIQUE (slug);

CREATE TABLE route_pauses (
    route text PRIMARY KEY,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    CONSTRAINT route_pauses_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    )
);

GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE route_pauses TO topup_app;
REVOKE TRUNCATE ON TABLE route_pauses FROM topup_app;
