# P2.1 inbox runtime attachment

This dark slice adds `DatabaseInboxStorage::attach_existing` for a database
whose inbox schema has already been prepared by the owner. Existing `open` and
`from_database` constructors still run owner bootstrap, and normal startup is
unchanged.

The new constructor accepts a caller-supplied `Database` and never opens a DSN.
For PostgreSQL it requires the main lane of a physically attested pool, repeats
attestation inside a read-only repeatable-read transaction, and qualifies schema
checks with the attested schema. It checks the required relation columns, ordered
primary keys, the `last_updated` width, recovery message-key nullability, and
the presence of valid, ready named indexes on the expected tables. SQLite checks
the corresponding local schema and refuses a temporary shadow on the connection
used for the check.

These checks describe one snapshot. They do not verify index definitions, prove
that a multi-statement owner job completed, establish the provenance of an
expected identity, audit all runtime grants, or prevent later schema changes.
The SQLite shadow check cannot inspect other pooled connections. The verified
global attachment remains opaque; this API does not provide a route to its
database, switch credentials, or authorize production activation. Portable and
clustered SM and pending-delivery attachment remain separate work.
