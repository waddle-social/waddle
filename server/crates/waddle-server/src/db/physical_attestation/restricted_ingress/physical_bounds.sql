-- This separate pass bounds physical source storage before the logical source
-- length check. Compressed known functions are permitted; the following
-- logical check uses PG17's text octet-length builtin, which reads TOAST size
-- metadata without decompressing. Reject compressed trigger argument arrays.
SELECT NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(p.proname,'waddle_ingress_')
    OR pg_catalog.starts_with(p.proname,'waddle_p21_')
    OR p.proname='pending_notification_recovery_ordinal_immutable')
    AND pg_catalog.pg_column_size(p.prosrc)>32768
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_trigger t
  JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid
  JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(c.relname,'ingress_')
    OR c.relname IN ('sessions','pending_delivery','p21_receipt_operation_results',
                     'singleton_candidate_slot','singleton_candidate_leases'))
    AND (pg_catalog.pg_column_compression(t.tgargs) IS NOT NULL OR pg_catalog.pg_column_size(t.tgargs)>8192)
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_constraint c
  JOIN pg_catalog.pg_class r ON r.oid=c.conrelid
  JOIN pg_catalog.pg_namespace n ON n.oid=r.relnamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(r.relname,'ingress_')
    OR r.relname IN ('sessions','pending_delivery','p21_receipt_operation_results',
                     'singleton_candidate_slot','singleton_candidate_leases'))
    AND c.conbin IS NOT NULL AND pg_catalog.pg_column_size(c.conbin)>32768
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_attrdef d JOIN pg_catalog.pg_class r ON r.oid=d.adrelid
  JOIN pg_catalog.pg_namespace n ON n.oid=r.relnamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(r.relname,'ingress_')
    OR r.relname IN ('sessions','pending_delivery','p21_receipt_operation_results',
                     'singleton_candidate_slot','singleton_candidate_leases'))
    AND pg_catalog.pg_column_size(d.adbin)>32768
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class r ON r.oid=i.indrelid
  JOIN pg_catalog.pg_namespace n ON n.oid=r.relnamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(r.relname,'ingress_')
    OR r.relname IN ('sessions','pending_delivery','p21_receipt_operation_results',
                     'singleton_candidate_slot','singleton_candidate_leases'))
    AND ((i.indexprs IS NOT NULL AND pg_catalog.pg_column_size(i.indexprs)>32768)
      OR (i.indpred IS NOT NULL AND pg_catalog.pg_column_size(i.indpred)>32768))
)
