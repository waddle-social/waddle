-- Run as a separate statement before inspecting any variable-length values.
-- Each subquery stops after its limit + 1, including on hostile catalogs.
WITH relations AS MATERIALIZED (
  SELECT c.oid FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(c.relname,'ingress_')
    OR c.relname IN ('sessions','pending_delivery','p21_receipt_operation_results',
                     'singleton_candidate_slot','singleton_candidate_leases'))
    AND c.relkind NOT IN ('i','I') LIMIT 65
)
SELECT (SELECT count(*) FROM (SELECT 1 FROM relations LIMIT 65) b) <= 64
  AND (SELECT count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
       JOIN relations r ON r.oid=a.attrelid LIMIT 2049) b) <= 2048
  AND (SELECT count(*) FROM (SELECT 1 FROM pg_catalog.pg_constraint c
       JOIN relations r ON r.oid=c.conrelid LIMIT 513) b) <= 512
  AND (SELECT count(*) FROM (SELECT 1 FROM pg_catalog.pg_trigger t
       JOIN relations r ON r.oid=t.tgrelid LIMIT 513) b) <= 512
  AND (SELECT count(*) FROM (SELECT 1 FROM pg_catalog.pg_index i
       JOIN relations r ON r.oid=i.indrelid LIMIT 513) b) <= 512
  AND (SELECT count(*) FROM (SELECT 1 FROM pg_catalog.pg_attrdef d
       JOIN relations r ON r.oid=d.adrelid LIMIT 513) b) <= 512
  AND (SELECT count(*) FROM (SELECT 1 FROM pg_catalog.pg_proc p
       JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
       WHERE n.nspname=$1 AND (pg_catalog.starts_with(p.proname,'waddle_ingress_')
         OR pg_catalog.starts_with(p.proname,'waddle_p21_')
         OR p.proname='pending_notification_recovery_ordinal_immutable') LIMIT 257) b) <= 256
