# Database migrations

`transitions` and `audit` are protected by `BEFORE UPDATE OR DELETE` triggers. A trigger is used
instead of grants on a dedicated application role because the service does not yet own database
role provisioning, and grants would not protect table owners or migration-time connections. The
trigger keeps the append-only invariant local to the schema and applies to every role. Integration
tests exercise both forbidden operations on both tables.
