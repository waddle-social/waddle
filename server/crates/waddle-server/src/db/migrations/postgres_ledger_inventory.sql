-- Bound catalog collections before the exact catalog query examines them.
SELECT
  (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid = $1 LIMIT 33) bounded) <= 32
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_index i
    WHERE i.indrelid = $1 LIMIT 2) bounded) <= 1
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_constraint c
    WHERE c.conrelid = $1 LIMIT 2) bounded) <= 1
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attrdef d
    WHERE d.adrelid = $1 LIMIT 2) bounded) <= 1
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = $1 LIMIT 1)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class = $1 LIMIT 1)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = $1 LIMIT 1)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid = $1 LIMIT 1)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhparent = $1 LIMIT 1)
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amop o
    WHERE o.amopfamily = 1976::pg_catalog.oid LIMIT 129) bounded) <= 128
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amproc p
    WHERE p.amprocfamily = 1976::pg_catalog.oid LIMIT 129) bounded) <= 128
