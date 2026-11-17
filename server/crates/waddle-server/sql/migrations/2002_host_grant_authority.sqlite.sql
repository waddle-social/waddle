-- Host-grant authority (ADR-0020 D5) is PostgreSQL only: SQLite holds no
-- canonical grant store, so no scope can be enrolled on it and every room
-- keeps its existing admission. The tables are listed as PostgreSQL-only in
-- scripts/schema-engine-differences.txt.
SELECT 1;
