-- PG17 structural contract; exclude physical OIDs, planner statistics and the
-- stored default of a column added to a populated table (atthasmissing,
-- attmissingval), which a database built from the retired migration history
-- holds and one built from the schema baseline does not.
-- All deparsers run with search_path=pg_catalog,pg_temp in both producer and verifier.
SELECT COALESCE(pg_catalog.jsonb_agg(definition ORDER BY definition->>'name'),'[]'::jsonb)
FROM (
  SELECT pg_catalog.jsonb_build_object(
    'name',c.relname,
    'properties',pg_catalog.jsonb_build_object('access_method',am.amname,
      'replica_identity',c.relreplident,'options',c.reloptions,'tablespace',c.reltablespace,
      'of_type',c.reloftype,'has_rules',c.relhasrules),
    -- relhassubclass is a sticky planner hint; pin live edges in either direction.
    'inheritance_edges',(SELECT count(*) FROM pg_catalog.pg_inherits i WHERE i.inhrelid=c.oid OR i.inhparent=c.oid),
    'columns',COALESCE((SELECT pg_catalog.jsonb_agg(
      pg_catalog.jsonb_build_object('properties',pg_catalog.to_jsonb(a)-ARRAY['attrelid','attstattarget','attcacheoff','attacl','atthasmissing','attmissingval'],
        'default',pg_catalog.pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum)
      FROM pg_catalog.pg_attribute a LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
      WHERE a.attrelid=c.oid AND a.attnum>0),'[]'::jsonb),
    'constraints',COALESCE((SELECT pg_catalog.jsonb_agg(
      pg_catalog.jsonb_build_object('properties',pg_catalog.to_jsonb(con)-ARRAY['oid','connamespace','conrelid','confrelid','conindid','conparentid','conbin'],
        'definition',pg_catalog.pg_get_constraintdef(con.oid),
        'foreign_schema',fn.nspname,'foreign_table',fc.relname,
        'index',ic.relname,'parent',con.conparentid<>0) ORDER BY con.conname)
      FROM pg_catalog.pg_constraint con
      LEFT JOIN pg_catalog.pg_class fc ON fc.oid=con.confrelid
      LEFT JOIN pg_catalog.pg_namespace fn ON fn.oid=fc.relnamespace
      LEFT JOIN pg_catalog.pg_class ic ON ic.oid=con.conindid
      WHERE con.conrelid=c.oid),'[]'::jsonb),
    'indexes',COALESCE((SELECT pg_catalog.jsonb_agg(
      pg_catalog.jsonb_build_object('properties',pg_catalog.to_jsonb(i)-ARRAY['indexrelid','indrelid','indexprs','indpred'],
        'name',ic.relname,'definition',pg_catalog.pg_get_indexdef(i.indexrelid),
        'options',ic.reloptions,'tablespace',ic.reltablespace) ORDER BY ic.relname)
      FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ic ON ic.oid=i.indexrelid
      WHERE i.indrelid=c.oid),'[]'::jsonb),
    'triggers',COALESCE((SELECT pg_catalog.jsonb_agg(def ORDER BY def::text)
      FROM (SELECT pg_catalog.jsonb_build_object(
        'properties',pg_catalog.to_jsonb(t)-ARRAY['oid','tgrelid','tgfoid','tgconstraint','tgconstrrelid','tgconstrindid','tgparentid','tgname','tgqual'],
        'name',CASE WHEN t.tgisinternal THEN pg_catalog.regexp_replace(t.tgname,'[0-9]+$','<oid>') ELSE t.tgname::text END,
        'when',pg_catalog.pg_get_expr(t.tgqual,t.tgrelid),
        'function_schema',pn.nspname,'function',p.proname,'function_args',p.proargtypes::text,
        'constraint',con.conname,'related_schema',rn.nspname,'related_table',rc.relname,
        'index',ic.relname,'parent',t.tgparentid<>0) AS def
        FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_proc p ON p.oid=t.tgfoid
        JOIN pg_catalog.pg_namespace pn ON pn.oid=p.pronamespace
        LEFT JOIN pg_catalog.pg_constraint con ON con.oid=t.tgconstraint
        LEFT JOIN pg_catalog.pg_class rc ON rc.oid=t.tgconstrrelid
        LEFT JOIN pg_catalog.pg_namespace rn ON rn.oid=rc.relnamespace
        LEFT JOIN pg_catalog.pg_class ic ON ic.oid=t.tgconstrindid
        WHERE t.tgrelid=c.oid) triggers),'[]'::jsonb),
    'rules', (SELECT count(*) FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class=c.oid)
  ) AS definition
  FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  LEFT JOIN pg_catalog.pg_am am ON am.oid=c.relam
  WHERE n.nspname=$1 AND c.relname=ANY($2)
) definitions
