-- Called only after bounded inputs and built-in expression/index dependency
-- verification. These are the exact definitions produced by the owner schema.
WITH expected(rel, name, kind, key, index_oid, definition) AS (
  VALUES ($1::oid,'singleton_candidate_slot_pkey','p',ARRAY[1]::smallint[],$3::oid,'PRIMARY KEY (slot)'),
         ($1::oid,'singleton_candidate_slot_slot_check','c',ARRAY[1]::smallint[],0::oid,'CHECK ((slot = 1))'),
         ($1::oid,'singleton_candidate_slot_check','c',ARRAY[2,3,4]::smallint[],0::oid,
          'CHECK ((((node_id IS NULL) AND (incarnation IS NULL) AND (generation = 0)) OR ((node_id IS NOT NULL) AND (incarnation IS NOT NULL) AND (generation > 0))))'),
         ($2::oid,'singleton_candidate_leases_pkey','p',ARRAY[1]::smallint[],$4::oid,'PRIMARY KEY (incarnation)'),
         ($2::oid,'singleton_candidate_leases_generation_key','u',ARRAY[3]::smallint[],$5::oid,'UNIQUE (generation)'),
         ($2::oid,'singleton_candidate_leases_node_id_incarnation_generation_key','u',ARRAY[2,1,3]::smallint[],$6::oid,'UNIQUE (node_id, incarnation, generation)'),
         ($2::oid,'singleton_candidate_leases_generation_check','c',ARRAY[3]::smallint[],0::oid,'CHECK ((generation > 0))'),
         ($2::oid,'singleton_candidate_leases_node_id_check','c',ARRAY[2]::smallint[],0::oid,
          'CHECK (((octet_length(node_id) >= 1) AND (octet_length(node_id) <= 255)))')
), fk AS (
  SELECT c.oid FROM pg_catalog.pg_constraint c
  WHERE c.conrelid=$1 AND c.conname='singleton_candidate_slot_node_id_incarnation_generation_fkey'
    AND c.contype='f' AND c.confrelid=$2 AND c.conindid=$6
    AND c.conkey=ARRAY[2,3,4]::smallint[] AND c.confkey=ARRAY[2,1,3]::smallint[]
    AND c.confupdtype='a' AND c.confdeltype='a' AND c.confmatchtype='s'
    AND c.convalidated AND NOT c.condeferrable AND NOT c.condeferred
    AND c.conislocal AND c.conparentid=0
)
SELECT (SELECT count(*) FROM pg_catalog.pg_constraint c WHERE c.conrelid IN ($1,$2))=9
  AND (SELECT count(*) FROM fk)=1
  AND NOT EXISTS (
    SELECT 1 FROM expected e LEFT JOIN pg_catalog.pg_constraint c
      ON c.conrelid=e.rel AND c.conname=e.name
    WHERE c.oid IS NULL OR c.contype::text<>e.kind OR c.conkey<>e.key OR c.conindid<>e.index_oid
       OR NOT c.convalidated OR c.condeferrable OR c.condeferred OR NOT c.conislocal OR c.conparentid<>0
       OR pg_catalog.pg_get_constraintdef(c.oid)<>e.definition
  )
  AND (SELECT count(*) FROM pg_catalog.pg_attrdef d WHERE d.adrelid IN ($1,$2))=1
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d
       WHERE d.adrelid=$2 AND d.adnum=5 AND pg_catalog.pg_get_expr(d.adbin,d.adrelid)='false')
  AND (SELECT count(*) FROM pg_catalog.pg_trigger t WHERE t.tgrelid IN ($1,$2))=4
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid IN ($1,$2)
      AND (NOT t.tgisinternal OR t.tgenabled<>'O' OR t.tgconstraint<>(SELECT oid FROM fk)
        OR t.tgqual IS NOT NULL OR pg_catalog.octet_length(t.tgargs)<>0
        OR NOT ((t.tgrelid=$1 AND t.tgfoid=1644::oid AND t.tgtype=5)
             OR (t.tgrelid=$1 AND t.tgfoid=1645::oid AND t.tgtype=17)
             OR (t.tgrelid=$2 AND t.tgfoid=1654::oid AND t.tgtype=9)
             OR (t.tgrelid=$2 AND t.tgfoid=1655::oid AND t.tgtype=17)))
  )
