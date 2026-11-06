-- pg_depend records non-pinned dependencies of expression-bearing catalog
-- objects. The built-in pg_catalog objects are the only permitted dependencies
-- in expressions the structural verifier will deparse. Trigger WHEN clauses
-- are rejected separately, so trigger-function dependencies are excluded.
WITH relations AS (
  SELECT c.oid FROM pg_catalog.pg_class c
  JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  WHERE n.nspname=$1 AND c.relname=ANY($2)
), objects AS (
  SELECT 'pg_catalog.pg_attrdef'::regclass AS classid,d.oid AS objid
  FROM pg_catalog.pg_attrdef d JOIN relations r ON r.oid=d.adrelid
  UNION ALL
  SELECT 'pg_catalog.pg_constraint'::regclass,con.oid
  FROM pg_catalog.pg_constraint con JOIN relations r ON r.oid=con.conrelid
  UNION ALL
  SELECT 'pg_catalog.pg_class'::regclass,i.indexrelid
  FROM pg_catalog.pg_index i JOIN relations r ON r.oid=i.indrelid
)
SELECT EXISTS (
  SELECT 1 FROM objects o JOIN pg_catalog.pg_depend d ON d.classid=o.classid AND d.objid=o.objid
  WHERE (d.refclassid='pg_catalog.pg_proc'::regclass AND NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
           WHERE p.oid=d.refobjid AND n.nspname='pg_catalog'))
     OR (d.refclassid='pg_catalog.pg_operator'::regclass AND NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_operator op JOIN pg_catalog.pg_namespace n ON n.oid=op.oprnamespace
           WHERE op.oid=d.refobjid AND n.nspname='pg_catalog'))
     OR (d.refclassid='pg_catalog.pg_type'::regclass AND NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace
           WHERE t.oid=d.refobjid AND n.nspname='pg_catalog'))
     OR (d.refclassid='pg_catalog.pg_collation'::regclass AND NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_collation col JOIN pg_catalog.pg_namespace n ON n.oid=col.collnamespace
           WHERE col.oid=d.refobjid AND n.nspname='pg_catalog'))
     OR (d.refclassid='pg_catalog.pg_opclass'::regclass AND NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_opclass oc JOIN pg_catalog.pg_namespace n ON n.oid=oc.opcnamespace
           WHERE oc.oid=d.refobjid AND n.nspname='pg_catalog'))
     OR (d.refclassid='pg_catalog.pg_opfamily'::regclass AND NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_opfamily ofam JOIN pg_catalog.pg_namespace n ON n.oid=ofam.opfnamespace
           WHERE ofam.oid=d.refobjid AND n.nspname='pg_catalog'))
) OR EXISTS (
  SELECT 1 FROM relations r
  JOIN pg_catalog.pg_index i ON i.indrelid=r.oid
  CROSS JOIN LATERAL pg_catalog.unnest(i.indclass) AS classes(opclass_oid)
  LEFT JOIN pg_catalog.pg_opclass oc ON oc.oid=classes.opclass_oid
  LEFT JOIN pg_catalog.pg_namespace n ON n.oid=oc.opcnamespace
  WHERE n.nspname IS DISTINCT FROM 'pg_catalog'
)
OR EXISTS (
  -- pg_get_indexdef invokes the access method handler. Every v2 index is
  -- btree; reject other handlers before asking PostgreSQL to deparse one.
  SELECT 1 FROM relations r
  JOIN pg_catalog.pg_index i ON i.indrelid=r.oid
  JOIN pg_catalog.pg_class ic ON ic.oid=i.indexrelid
  LEFT JOIN pg_catalog.pg_am am ON am.oid=ic.relam
  WHERE ic.relam<>403::oid OR am.amhandler<>330::oid
    OR am.amname IS DISTINCT FROM 'btree' OR am.amtype IS DISTINCT FROM 'i'
)
OR EXISTS (
  -- The pinned profile uses only the default collation for text index keys.
  SELECT 1 FROM relations r
  JOIN pg_catalog.pg_index i ON i.indrelid=r.oid
  CROSS JOIN LATERAL pg_catalog.unnest(i.indcollation) AS collations(collation_oid)
  WHERE collations.collation_oid NOT IN (0::oid,100::oid)
)
