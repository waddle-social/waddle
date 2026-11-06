-- Catalog-only shape used before reading the guard manifest or invoking a
-- PostgreSQL expression deparser. No application relation is read here.
SELECT COALESCE(pg_catalog.jsonb_agg(definition ORDER BY definition->>'name'),'[]'::jsonb)
FROM (
  SELECT pg_catalog.jsonb_build_object(
    'name',c.relname,
    'columns',COALESCE((SELECT pg_catalog.jsonb_agg(
      pg_catalog.jsonb_build_object('attname',a.attname,'attnum',a.attnum,
        'atttypid',a.atttypid::text,'attcollation',a.attcollation::text,
        'attisdropped',a.attisdropped) ORDER BY a.attnum)
      FROM pg_catalog.pg_attribute a WHERE a.attrelid=c.oid AND a.attnum>0),'[]'::jsonb),
    'triggers',COALESCE((SELECT pg_catalog.jsonb_agg(def ORDER BY def::text)
      FROM (SELECT pg_catalog.jsonb_build_object(
        'properties',pg_catalog.to_jsonb(t)-ARRAY['oid','tgrelid','tgfoid','tgconstraint','tgconstrrelid','tgconstrindid','tgparentid','tgname','tgqual'],
        'name',CASE WHEN t.tgisinternal THEN pg_catalog.regexp_replace(t.tgname,'[0-9]+$','<oid>') ELSE t.tgname::text END,
        'when',NULL,
        'function_schema',pn.nspname,'function',p.proname,'function_args',p.proargtypes::text,
        'constraint',con.conname,'related_schema',rn.nspname,'related_table',rc.relname,
        'index',ic.relname,'parent',t.tgparentid<>0) AS def
        FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_proc p ON p.oid=t.tgfoid
        JOIN pg_catalog.pg_namespace pn ON pn.oid=p.pronamespace
        LEFT JOIN pg_catalog.pg_constraint con ON con.oid=t.tgconstraint
        LEFT JOIN pg_catalog.pg_class rc ON rc.oid=t.tgconstrrelid
        LEFT JOIN pg_catalog.pg_namespace rn ON rn.oid=rc.relnamespace
        LEFT JOIN pg_catalog.pg_class ic ON ic.oid=t.tgconstrindid
        WHERE t.tgrelid=c.oid) triggers),'[]'::jsonb)
  ) AS definition
  FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  WHERE n.nspname=$1 AND c.relname=ANY($2)
) definitions
