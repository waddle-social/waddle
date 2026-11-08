-- PG17-only append profile for the two MAM heaps. The preceding statement
-- bounds variable-length catalog data before any ACL or array expansion.
WITH input AS MATERIALIZED (
  SELECT $1::pg_catalog.oid AS messages, $2::pg_catalog.oid AS sequences,
         $3::pg_catalog.oid AS schema_oid, $4::pg_catalog.oid AS owner_oid,
         $5::pg_catalog.oid AS runtime_oid, $6::pg_catalog.oid AS index_oid,
         $7::pg_catalog.text AS schema_name
), targets AS MATERIALIZED (
  SELECT c.* FROM pg_catalog.pg_class c, input p
  WHERE c.oid IN (p.messages,p.sequences)
)
SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4 BETWEEN 170000 AND 179999
  AND (SELECT pg_catalog.count(*) FROM targets)=2
  AND EXISTS (
    SELECT 1 FROM pg_catalog.pg_namespace n, input p
    WHERE n.oid=p.schema_oid AND n.nspname=p.schema_name AND n.nspowner=p.owner_oid
  )
  AND EXISTS (
    SELECT 1 FROM pg_catalog.pg_roles r, pg_catalog.pg_database d, input p
    WHERE r.oid=p.runtime_oid AND d.datname=pg_catalog.current_database()
      AND r.rolname=session_user AND r.rolname=current_user
      AND r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole
      AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls
      AND r.oid<>p.owner_oid AND r.oid<>d.datdba
      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member=r.oid)
      AND NOT pg_catalog.has_database_privilege(r.oid,d.oid,'CREATE,TEMP')
      AND pg_catalog.has_schema_privilege(r.oid,p.schema_oid,'USAGE')
      AND NOT pg_catalog.has_schema_privilege(r.oid,p.schema_oid,'CREATE')
      AND NOT pg_catalog.has_parameter_privilege(r.oid,'session_replication_role','SET')
      AND pg_catalog.current_setting('session_replication_role')='origin'
  )
  AND NOT EXISTS (
    SELECT 1 FROM targets c, input p
    WHERE c.relnamespace<>p.schema_oid OR c.relowner<>p.owner_oid
      OR c.relkind<>'r' OR c.relpersistence<>'p' OR c.relam<>2::pg_catalog.oid
      OR c.relispartition OR c.reloftype<>0::pg_catalog.oid
      OR c.relrowsecurity OR c.relforcerowsecurity OR c.relhasrules
      OR c.reloptions IS NOT NULL
      OR EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid=c.oid OR h.inhparent=c.oid)
      OR EXISTS (SELECT 1 FROM pg_catalog.pg_policy x WHERE x.polrelid=c.oid)
      OR EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite x WHERE x.ev_class=c.oid)
      OR EXISTS (SELECT 1 FROM pg_catalog.pg_trigger x WHERE x.tgrelid=c.oid)
  )
  AND EXISTS (
    SELECT 1 FROM targets c, input p
    WHERE c.oid=p.messages AND c.relname='mam_messages'
  )
  AND EXISTS (
    SELECT 1 FROM targets c, input p
    WHERE c.oid=p.sequences AND c.relname='mam_archive_sequences'
  )
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a, input p
       WHERE a.attrelid=p.sequences AND a.attnum>0 AND NOT a.attisdropped)=2
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a, input p
                  WHERE a.attrelid=p.sequences AND a.attnum>0 AND a.attisdropped)
  AND NOT EXISTS (
    SELECT 1 FROM (VALUES ('archive_jid',25::pg_catalog.oid,100::pg_catalog.oid),
                          ('next_seq',20::pg_catalog.oid,0::pg_catalog.oid)) e(name,typ,coll),
                  input p
    WHERE NOT EXISTS (
      SELECT 1 FROM pg_catalog.pg_attribute a
      WHERE a.attrelid=p.sequences AND a.attnum>0 AND NOT a.attisdropped
        AND a.attname=e.name AND a.atttypid=e.typ AND a.attcollation=e.coll
        AND a.attnotnull AND a.atttypmod=-1 AND a.attndims=0
        AND a.attislocal AND a.attinhcount=0 AND NOT a.atthasmissing
        AND a.attgenerated='' AND a.attidentity=''
        AND a.attoptions IS NULL AND a.attfdwoptions IS NULL
    )
  )
  AND EXISTS (
    SELECT 1 FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class idx ON idx.oid=i.indexrelid
    JOIN pg_catalog.pg_am am ON am.oid=idx.relam
    JOIN pg_catalog.pg_attribute a ON a.attrelid=i.indrelid AND a.attnum=i.indkey[0],
    input p
    WHERE i.indrelid=p.sequences AND idx.oid=p.index_oid
      AND idx.relname='mam_archive_sequences_pkey' AND idx.relnamespace=p.schema_oid
      AND idx.relowner=p.owner_oid AND idx.relkind='i' AND idx.relpersistence='p'
      AND idx.relam=403::pg_catalog.oid AND idx.reloptions IS NULL
      AND am.amname='btree' AND am.amtype='i' AND am.amhandler=330::pg_catalog.oid
      AND i.indisunique AND i.indisprimary AND i.indisvalid AND i.indisready
      AND i.indislive AND i.indimmediate AND NOT i.indisexclusion
      AND NOT i.indnullsnotdistinct AND i.indnatts=1 AND i.indnkeyatts=1
      AND i.indexprs IS NULL AND i.indpred IS NULL AND a.attname='archive_jid'
      AND i.indclass[0]=3126::pg_catalog.oid AND i.indcollation[0]=100::pg_catalog.oid
      AND i.indoption[0]=0
  )
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class idx ON idx.oid=i.indexrelid, input p
    WHERE i.indrelid IN (p.messages,p.sequences)
      AND (idx.reloptions IS NOT NULL OR i.indisclustered OR i.indisreplident)
  )
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_statistic_ext s, input p
                  WHERE s.stxrelid IN (p.messages,p.sequences))
  -- Both heaps have only their immediate primary-key constraints. Incoming
  -- foreign keys are also refused, including their internal trigger edges.
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint c, input p
       WHERE c.conrelid IN (p.messages,p.sequences))=2
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_constraint c, input p
    WHERE c.confrelid IN (p.messages,p.sequences)
       OR (c.conrelid IN (p.messages,p.sequences) AND NOT (
         c.contype='p' AND NOT c.condeferrable AND NOT c.condeferred AND c.convalidated
         AND c.conkey=ARRAY[1]::pg_catalog.int2[]
         AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i
                     WHERE i.indrelid=c.conrelid AND i.indexrelid=c.conindid AND i.indisprimary)
         AND ((c.conrelid=p.messages AND c.conname='mam_messages_pkey')
           OR (c.conrelid=p.sequences AND c.conname='mam_archive_sequences_pkey'))
       ))
  )
  -- Match the two built-in PG17 expression trees bytewise without deparsing
  -- any attacker-supplied expression. Both trees were observed from real DDL.
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attrdef d, input p
       WHERE d.adrelid IN (p.messages,p.sequences))=2
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_attrdef d
    JOIN pg_catalog.pg_attribute a ON a.attrelid=d.adrelid AND a.attnum=d.adnum, input p
    WHERE d.adrelid IN (p.messages,p.sequences) AND NOT (
      d.adrelid=p.messages AND (
        (a.attname='message_type' AND pg_catalog.convert_to(d.adbin::pg_catalog.text,'UTF8')
          OPERATOR(pg_catalog.=) pg_catalog.convert_to(
          '{CONST :consttype 25 :consttypmod -1 :constcollid 100 :constlen -1 :constbyval false :constisnull false :location -1 :constvalue 10 [ 40 0 0 0 110 111 114 109 97 108 ]}','UTF8'))
        OR (a.attname='created_at' AND pg_catalog.convert_to(d.adbin::pg_catalog.text,'UTF8')
          OPERATOR(pg_catalog.=) pg_catalog.convert_to(
          '{SQLVALUEFUNCTION :op 3 :type 1184 :typmod -1 :location -1}','UTF8'))
      )
    )
  )
  -- Direct grants are restricted to owner and runtime. Exact effective rights
  -- are checked independently so inherited or PUBLIC excess cannot pass.
  AND NOT EXISTS (
    SELECT 1 FROM targets c, input p
    WHERE NOT pg_catalog.has_table_privilege(p.runtime_oid,c.oid,'SELECT')
       OR NOT pg_catalog.has_table_privilege(p.runtime_oid,c.oid,'INSERT')
       OR pg_catalog.has_table_privilege(p.runtime_oid,c.oid,'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR pg_catalog.has_table_privilege(p.runtime_oid,c.oid,
          'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION,MAINTAIN WITH GRANT OPTION')
       OR pg_catalog.has_any_column_privilege(p.runtime_oid,c.oid,
          'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION')
       OR pg_catalog.has_any_column_privilege(p.runtime_oid,c.oid,'REFERENCES')
       OR (c.oid=p.messages AND pg_catalog.has_any_column_privilege(p.runtime_oid,c.oid,'UPDATE'))
       OR (c.oid=p.sequences AND (
         NOT pg_catalog.has_column_privilege(p.runtime_oid,c.oid,'next_seq','UPDATE')
         OR pg_catalog.has_column_privilege(p.runtime_oid,c.oid,'archive_jid','UPDATE')))
       OR NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
          WHERE acl.grantee=p.runtime_oid AND acl.grantor=p.owner_oid
            AND acl.privilege_type='SELECT' AND NOT acl.is_grantable)
       OR NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
          WHERE acl.grantee=p.runtime_oid AND acl.grantor=p.owner_oid
            AND acl.privilege_type='INSERT' AND NOT acl.is_grantable)
       OR EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
            COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
          WHERE acl.grantee<>p.owner_oid AND NOT (
            acl.grantee=p.runtime_oid AND acl.grantor=p.owner_oid AND NOT acl.is_grantable
            AND acl.privilege_type IN ('SELECT','INSERT')))
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
          CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
          WHERE a.attrelid=c.oid AND acl.grantee<>p.owner_oid AND NOT (
            c.oid=p.sequences AND a.attname='next_seq'
            AND acl.grantee=p.runtime_oid AND acl.grantor=p.owner_oid
            AND acl.privilege_type='UPDATE' AND NOT acl.is_grantable))
  )
  AND EXISTS (
    SELECT 1 FROM pg_catalog.pg_attribute a
    CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl, input p
    WHERE a.attrelid=p.sequences AND a.attname='next_seq'
      AND acl.grantee=p.runtime_oid AND acl.grantor=p.owner_oid
      AND acl.privilege_type='UPDATE' AND NOT acl.is_grantable
  )
