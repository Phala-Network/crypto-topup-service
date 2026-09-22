DROP TABLE route_pauses;

ALTER TABLE products
    DROP CONSTRAINT products_slug_unique;
