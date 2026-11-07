-- Independently pinned singleton heaps, indexes and effective privileges.
-- The caller has already bounded catalog inputs and holds the catalog fence.
WITH targets AS (
  SELECT c.*, n.nspowner FROM pg_catalog.pg_class c
  JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
  WHERE n.nspname=$1 AND n.oid=$2 AND n.nspowner=$3
    AND ((c.oid=$6 AND c.relname='singleton_candidate_slot')
      OR (c.oid=$8 AND c.relname='singleton_candidate_leases'))
), expected_columns(rel, num, name, typ, required, has_default, collid) AS (
  VALUES ($6::oid,1,'slot',21::oid,true,false,0::oid),
         ($6::oid,2,'node_id',25::oid,false,false,100::oid),
         ($6::oid,3,'incarnation',2950::oid,false,false,0::oid),
         ($6::oid,4,'generation',20::oid,true,false,0::oid),
         ($8::oid,1,'incarnation',2950::oid,true,false,0::oid),
         ($8::oid,2,'node_id',25::oid,true,false,100::oid),
         ($8::oid,3,'generation',20::oid,true,false,0::oid),
         ($8::oid,4,'expires_at',1184::oid,true,false,0::oid),
         ($8::oid,5,'revoked',16::oid,true,true,0::oid)
), expected_indexes(rel, idx, key, class, coll, options, is_primary) AS (
  VALUES ($6::oid,$7::oid,'1','1979','0','0',true),
         ($8::oid,$9::oid,'1','10065','0','0',true),
         ($8::oid,$10::oid,'3','3124','0','0',false),
         ($8::oid,$11::oid,'2 1 3','3126 10065 3124','100 0 0','0 0 0',false)
)
SELECT (SELECT count(*) FROM targets)=2
  AND NOT EXISTS (
    SELECT 1 FROM targets c
    WHERE c.relowner<>$3 OR c.relkind<>'r' OR c.relpersistence<>'p'
       OR c.relam<>2::oid OR c.relispartition OR c.relrowsecurity OR c.relforcerowsecurity
       OR c.relhasrules OR c.reloftype<>0
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid=c.oid OR i.inhparent=c.oid)
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid)
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite r WHERE r.ev_class=c.oid)
       OR pg_catalog.has_table_privilege($4,c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR pg_catalog.has_any_column_privilege($4,c.oid,'SELECT,INSERT,UPDATE,REFERENCES')
       OR NOT pg_catalog.has_table_privilege($5,c.oid,'SELECT')
       OR pg_catalog.has_table_privilege($5,c.oid,'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR pg_catalog.has_table_privilege($5,c.oid,'SELECT WITH GRANT OPTION')
       OR EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
          WHERE acl.grantee<>c.relowner AND
            (acl.grantor<>c.relowner OR acl.is_grantable OR acl.grantee<>$5 OR acl.privilege_type<>'SELECT'))
  )
  AND (SELECT count(*) FROM pg_catalog.pg_attribute a JOIN targets c ON c.oid=a.attrelid WHERE a.attnum>0)=9
  AND NOT EXISTS (
    SELECT 1 FROM expected_columns e LEFT JOIN pg_catalog.pg_attribute a
      ON a.attrelid=e.rel AND a.attnum=e.num
    WHERE a.attname IS DISTINCT FROM e.name OR a.atttypid IS DISTINCT FROM e.typ
       OR a.attnotnull IS DISTINCT FROM e.required OR a.atthasdef IS DISTINCT FROM e.has_default
       OR a.attcollation IS DISTINCT FROM e.collid OR a.attisdropped OR a.atthasmissing
       OR a.attgenerated<>'' OR a.attidentity<>'' OR a.attndims<>0 OR a.atttypmod<>-1
       OR a.attoptions IS NOT NULL OR a.attfdwoptions IS NOT NULL
       OR (a.attrelid=$8 AND a.attacl IS NOT NULL)
       OR (a.attrelid=$6 AND EXISTS (
           SELECT 1 FROM pg_catalog.aclexplode(a.attacl) acl
           WHERE acl.grantee<>$3 AND NOT (
             a.attname='slot' AND acl.grantee=$5 AND acl.privilege_type='UPDATE'
             AND NOT acl.is_grantable AND acl.grantor=$3)))
       OR (a.attrelid=$6 AND a.attname<>'slot'
           AND pg_catalog.has_column_privilege($5,a.attrelid,a.attnum,'UPDATE'))
  )
  AND pg_catalog.has_column_privilege($5,$6,'slot','UPDATE')
  AND NOT pg_catalog.has_column_privilege($5,$6,'slot','UPDATE WITH GRANT OPTION')
  AND NOT pg_catalog.has_any_column_privilege($5,$8,'INSERT,UPDATE,REFERENCES')
  AND (SELECT count(*) FROM pg_catalog.pg_index i JOIN targets c ON c.oid=i.indrelid)=4
  AND NOT EXISTS (
    SELECT 1 FROM expected_indexes e
    LEFT JOIN pg_catalog.pg_index i ON i.indrelid=e.rel AND i.indexrelid=e.idx
    LEFT JOIN pg_catalog.pg_class idx ON idx.oid=i.indexrelid
    LEFT JOIN pg_catalog.pg_am am ON am.oid=idx.relam
    WHERE i.indexrelid IS NULL OR idx.relowner<>$3 OR idx.relkind<>'i' OR idx.relnamespace<>$2
       OR am.oid<>403::oid OR am.amhandler<>330::oid OR am.amname<>'btree'
       OR NOT i.indisunique OR NOT i.indisvalid OR NOT i.indisready OR NOT i.indislive
       OR NOT i.indimmediate OR i.indisexclusion OR i.indnullsnotdistinct
       OR i.indisprimary IS DISTINCT FROM e.is_primary OR i.indkey::text<>e.key
       OR i.indclass::text<>e.class OR i.indcollation::text<>e.coll OR i.indoption::text<>e.options
       OR i.indnatts<>CASE WHEN e.idx=$11 THEN 3 ELSE 1 END
       OR i.indnkeyatts<>i.indnatts OR i.indexprs IS NOT NULL OR i.indpred IS NOT NULL
  )
