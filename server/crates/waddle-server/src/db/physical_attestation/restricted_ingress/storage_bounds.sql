-- Inventory bounds were proven by the preceding, separate statement. Reject
-- compressed catalog arrays before aclexplode/JSON expansion, and bound source
-- by logical byte length before SQLx can decode it. PostgreSQL's octet_length
-- on text/bytea uses the datum's raw length, including toasted values.
WITH relations AS MATERIALIZED (
  SELECT c.oid FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(c.relname,'ingress_')
    OR c.relname IN ('sessions','pending_delivery','p21_receipt_operation_results',
                     'singleton_candidate_slot','singleton_candidate_leases'))
    AND c.relkind NOT IN ('i','I')
), functions AS MATERIALIZED (
  SELECT p.oid FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
  WHERE n.nspname=$1 AND (pg_catalog.starts_with(p.proname,'waddle_ingress_')
    OR pg_catalog.starts_with(p.proname,'waddle_p21_')
    OR p.proname='pending_notification_recovery_ordinal_immutable')
)
SELECT NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=$1 AND n.nspacl IS NOT NULL
    AND (pg_catalog.pg_column_compression(n.nspacl) IS NOT NULL OR pg_catalog.pg_column_size(n.nspacl)>8192)
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_database d WHERE d.datname=pg_catalog.current_database() AND d.datacl IS NOT NULL
    AND (pg_catalog.pg_column_compression(d.datacl) IS NOT NULL OR pg_catalog.pg_column_size(d.datacl)>8192)
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_class c JOIN relations r ON r.oid=c.oid
  WHERE (c.relacl IS NOT NULL AND (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL OR pg_catalog.pg_column_size(c.relacl)>8192))
     OR (c.reloptions IS NOT NULL AND (pg_catalog.pg_column_compression(c.reloptions) IS NOT NULL OR pg_catalog.pg_column_size(c.reloptions)>8192))
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_attribute a JOIN relations r ON r.oid=a.attrelid
  WHERE (a.attacl IS NOT NULL AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL OR pg_catalog.pg_column_size(a.attacl)>8192))
     OR (a.attoptions IS NOT NULL AND (pg_catalog.pg_column_compression(a.attoptions) IS NOT NULL OR pg_catalog.pg_column_size(a.attoptions)>8192))
     OR (a.attfdwoptions IS NOT NULL AND (pg_catalog.pg_column_compression(a.attfdwoptions) IS NOT NULL OR pg_catalog.pg_column_size(a.attfdwoptions)>8192))
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_proc p JOIN functions f ON f.oid=p.oid
  WHERE pg_catalog.octet_length(p.prosrc)>32768
     OR p.proargdefaults IS NOT NULL OR p.prosqlbody IS NOT NULL OR p.probin IS NOT NULL OR p.protrftypes IS NOT NULL
     OR (p.proacl IS NOT NULL AND (pg_catalog.pg_column_compression(p.proacl) IS NOT NULL OR pg_catalog.pg_column_size(p.proacl)>8192))
     OR (p.proconfig IS NOT NULL AND (pg_catalog.pg_column_compression(p.proconfig) IS NOT NULL OR pg_catalog.pg_column_size(p.proconfig)>8192))
     OR (p.proargnames IS NOT NULL AND (pg_catalog.pg_column_compression(p.proargnames) IS NOT NULL OR pg_catalog.pg_column_size(p.proargnames)>8192))
     OR (p.proallargtypes IS NOT NULL AND (pg_catalog.pg_column_compression(p.proallargtypes) IS NOT NULL OR pg_catalog.pg_column_size(p.proallargtypes)>8192))
     OR (p.proargmodes IS NOT NULL AND (pg_catalog.pg_column_compression(p.proargmodes) IS NOT NULL OR pg_catalog.pg_column_size(p.proargmodes)>8192))
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_trigger t JOIN relations r ON r.oid=t.tgrelid
  WHERE t.tgqual IS NOT NULL
     OR pg_catalog.pg_column_size(t.tgattr)>8192
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_constraint c JOIN relations r ON r.oid=c.conrelid
  WHERE (c.conbin IS NOT NULL AND pg_catalog.octet_length(c.conbin::text)>32768)
     OR (c.conkey IS NOT NULL AND (pg_catalog.pg_column_compression(c.conkey) IS NOT NULL OR pg_catalog.pg_column_size(c.conkey)>8192))
     OR (c.confkey IS NOT NULL AND (pg_catalog.pg_column_compression(c.confkey) IS NOT NULL OR pg_catalog.pg_column_size(c.confkey)>8192))
     OR (c.confdelsetcols IS NOT NULL AND (pg_catalog.pg_column_compression(c.confdelsetcols) IS NOT NULL OR pg_catalog.pg_column_size(c.confdelsetcols)>8192))
     OR (c.conpfeqop IS NOT NULL AND (pg_catalog.pg_column_compression(c.conpfeqop) IS NOT NULL OR pg_catalog.pg_column_size(c.conpfeqop)>8192))
     OR (c.conppeqop IS NOT NULL AND (pg_catalog.pg_column_compression(c.conppeqop) IS NOT NULL OR pg_catalog.pg_column_size(c.conppeqop)>8192))
     OR (c.conffeqop IS NOT NULL AND (pg_catalog.pg_column_compression(c.conffeqop) IS NOT NULL OR pg_catalog.pg_column_size(c.conffeqop)>8192))
     OR (c.conexclop IS NOT NULL AND (pg_catalog.pg_column_compression(c.conexclop) IS NOT NULL OR pg_catalog.pg_column_size(c.conexclop)>8192))
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_attrdef d JOIN relations r ON r.oid=d.adrelid
  WHERE pg_catalog.octet_length(d.adbin::text)>32768
) AND NOT EXISTS (
  SELECT 1 FROM pg_catalog.pg_index i JOIN relations r ON r.oid=i.indrelid
  JOIN pg_catalog.pg_class c ON c.oid=i.indexrelid
  WHERE (i.indexprs IS NOT NULL AND pg_catalog.octet_length(i.indexprs::text)>32768)
     OR (i.indpred IS NOT NULL AND pg_catalog.octet_length(i.indpred::text)>32768)
     OR (c.reloptions IS NOT NULL AND (pg_catalog.pg_column_compression(c.reloptions) IS NOT NULL OR pg_catalog.pg_column_size(c.reloptions)>8192))
)
