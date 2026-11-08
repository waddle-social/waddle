-- This uses catalog data only. In particular, do not deparse expressions:
-- pg_get_expr can invoke type output functions for hostile constants.
SELECT n.oid = $2 AND n.nspowner = $3 AND c.oid = $4 AND c.relowner = $5
  AND c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition
  AND c.relam = (SELECT am.oid FROM pg_catalog.pg_am am
                 WHERE am.amname = 'heap' AND am.amtype = 't')
  AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                  WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a
       WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped)
       = CASE WHEN $7 THEN 4 ELSE 3 END
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid = c.oid AND a.attnum > 0 AND (
      a.atthasmissing OR a.attgenerated <> '' OR a.attidentity <> ''
      OR a.attacl IS NOT NULL OR a.attoptions IS NOT NULL OR a.attfdwoptions IS NOT NULL
      OR (NOT a.attisdropped AND (
        a.attndims <> 0 OR a.atttypmod <> -1
        OR (a.attname, a.atttypid, a.attnotnull, a.attcollation, a.atthasdef) NOT IN (
          ('version', 20::pg_catalog.oid, true, 0::pg_catalog.oid, false),
          ('description', 25::pg_catalog.oid, true, 100::pg_catalog.oid, false),
          ('checksum', 25::pg_catalog.oid, false, 100::pg_catalog.oid, false),
          ('applied_at', 1184::pg_catalog.oid, true, 0::pg_catalog.oid, true))))))
  -- The sole default is the parser's built-in CURRENT_TIMESTAMP expression.
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attrdef d WHERE d.adrelid = c.oid) = 1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d
    JOIN pg_catalog.pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
    WHERE d.adrelid = c.oid AND a.attname = 'applied_at' AND NOT a.attisdropped
      AND d.adbin::text = '{SQLVALUEFUNCTION :op 3 :type 1184 :typmod -1 :location -1}')
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint k WHERE k.conrelid = c.oid) = 1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint k
    JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attname = 'version'
    WHERE k.conrelid = c.oid AND k.contype = 'p' AND k.conindid = $6
      AND k.conkey = ARRAY[a.attnum]::pg_catalog.int2[] AND k.convalidated
      AND NOT k.condeferrable AND NOT k.condeferred AND k.conislocal AND k.coninhcount = 0)
  -- Even a query without ORDER BY can have an index chosen by the planner.
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid) = 1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class idx ON idx.oid = i.indexrelid
    JOIN pg_catalog.pg_opclass opclass ON opclass.oid = i.indclass[0]
      AND opclass.oid = 3124::pg_catalog.oid
    JOIN pg_catalog.pg_opfamily family ON family.oid = opclass.opcfamily
      AND family.oid = 1976::pg_catalog.oid
    WHERE i.indrelid = c.oid AND idx.oid = $6 AND idx.relnamespace = n.oid
      AND idx.relowner = c.relowner AND idx.relkind = 'i' AND idx.relpersistence = 'p'
      AND idx.relam = 403::pg_catalog.oid AND idx.reloptions IS NULL
      AND opclass.opcmethod = 403::pg_catalog.oid AND opclass.opcnamespace = 11::pg_catalog.oid
      AND opclass.opcname = 'int8_ops' AND opclass.opcintype = 20::pg_catalog.oid
      AND opclass.opcdefault AND family.opfnamespace = 11::pg_catalog.oid
      AND family.opfname = 'integer_ops' AND family.opfmethod = 403::pg_catalog.oid
      AND i.indisprimary AND i.indisunique AND i.indisvalid AND i.indisready
      AND i.indislive AND i.indimmediate AND NOT i.indisexclusion
      AND NOT i.indnullsnotdistinct AND i.indnatts = 1 AND i.indnkeyatts = 1
      AND i.indkey[0] = (SELECT a.attnum FROM pg_catalog.pg_attribute a
                         WHERE a.attrelid = c.oid AND a.attname = 'version')
      AND i.indcollation[0] = 0::pg_catalog.oid AND i.indoption[0] = 0
      AND i.indexprs IS NULL AND i.indpred IS NULL
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amproc p
        JOIN pg_catalog.pg_proc f ON f.oid = p.amproc
        WHERE p.amprocfamily = 1976::pg_catalog.oid AND f.pronamespace <> 11::pg_catalog.oid)
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amop op
        JOIN pg_catalog.pg_operator o ON o.oid = op.amopopr
        JOIN pg_catalog.pg_proc f ON f.oid = o.oprcode
        WHERE op.amopfamily = 1976::pg_catalog.oid
          AND (o.oprnamespace <> 11::pg_catalog.oid OR f.pronamespace <> 11::pg_catalog.oid)))
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE n.nspname = $1 AND n.oid = $2 AND c.relname = '_migrations' AND c.oid = $4
