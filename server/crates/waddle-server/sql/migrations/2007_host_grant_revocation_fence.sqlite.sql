-- The host-grant revocation fence (ADR-0020 D5) belongs to the canonical
-- grant store, which is PostgreSQL only: SQLite enrolls no scope, so no
-- replica registers for one. The tables are listed as PostgreSQL-only in
-- scripts/schema-engine-differences.txt.
SELECT 1;
