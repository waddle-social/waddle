-- Execute only after storage_bounds.sql bounded physical and worst-case pglz
-- expansion. Arrays here are consequently safe to detoast for cardinality.
SELECT NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(p.proname,'waddle_ingress_')
    OR pg_catalog.starts_with(p.proname,'waddle_p21_')
    OR p.proname='pending_notification_recovery_ordinal_immutable')
    AND ((p.proacl IS NOT NULL AND pg_catalog.cardinality(p.proacl)>64)
      OR (p.proconfig IS NOT NULL AND pg_catalog.cardinality(p.proconfig)>32)
      OR (p.proargnames IS NOT NULL AND pg_catalog.cardinality(p.proargnames)>128)
      OR (p.proallargtypes IS NOT NULL AND pg_catalog.cardinality(p.proallargtypes)>128)
      OR (p.proargmodes IS NOT NULL AND pg_catalog.cardinality(p.proargmodes)>128))
)
