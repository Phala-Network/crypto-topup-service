CREATE TABLE lock_exposure (
    scope_key text PRIMARY KEY,
    open_minor numeric(78,0) NOT NULL CHECK (open_minor >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO lock_exposure (scope_key, open_minor)
SELECT scope.scope_key, sum(rate_lock.credit_minor)
FROM rate_locks AS rate_lock
JOIN addresses AS address ON address.id = rate_lock.address_id
JOIN accounts AS account ON account.id = address.account_id
CROSS JOIN LATERAL (
    VALUES ('account:' || account.id::text), ('product:' || account.product_id::text), ('global')
) AS scope (scope_key)
WHERE rate_lock.status = 'open' AND rate_lock.exposure_reserved
GROUP BY scope.scope_key;
