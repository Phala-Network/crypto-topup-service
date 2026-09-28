DELETE FROM permissions WHERE permission = 'deposits.write';
ALTER TABLE refunds DROP COLUMN metadata;
ALTER TABLE deposits DROP COLUMN metadata;
ALTER TABLE quotes DROP COLUMN metadata;
DROP FUNCTION metadata_is_valid(jsonb);
