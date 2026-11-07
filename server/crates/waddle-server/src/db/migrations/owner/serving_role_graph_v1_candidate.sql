-- UNREGISTERED candidate: v1 ingress/epoch/journal/PubSub portion of the
-- serving role graph. This is not an installer, a complete store-ACL matrix,
-- or permission to select a runtime. The trusted provisioner must render the
-- quoted identifier and escaped literal placeholders (restricted to the
-- existing simple identifier grammar `[a-z_][a-z0-9_]{0,62}`), and substitute the
-- exact pinned ingress_definer_api_candidate.sql and
-- p21_receipt_journal_lookup_candidate.sql assets at the two SQL placeholders.
-- Render __SCHEMA__ as a quoted identifier even for simple identifiers: the
-- H-helper profile verifies the exact resulting body. Never accept any of
-- these substitutions from R.
--
-- Run atomically in a caller-owned READ COMMITTED transaction as a trusted role
-- administrator, only after independently pinning the primary, schema, all
-- involved relation/function identities and bodies, ledger, and role OIDs;
-- fencing old login/reconnection and backends; and acquiring lineage, ledger,
-- then epoch ACCESS EXCLUSIVE under the trusted mutation protocol. The checks
-- below do not replace that catalog proof or prove it stays current. Unknown
-- COMMIT, journal admission/retention, every other store ACL, the H-aware
-- PubSub verifier, pool hooks and startup remain separate serving gates.
--
-- S = schema owner NOLOGIN; R = runtime LOGIN; A = fixed ingress API owner
-- NOLOGIN; H = fixed PubSub lock-helper owner NOLOGIN; T = attestor owner
-- NOLOGIN; I = attestor installer LOGIN; M = pg_monitor; X = migrator LOGIN.
-- I -> T and X -> S are the only SET-only membership edges involving these
-- principals. M may have other members, but none of S/R/A/H/T/I/X may use it.

-- Type names in the pinned assets are resolved when functions are created.
-- Put pg_catalog ahead of even an already-present installer temporary schema.
SET LOCAL search_path = pg_catalog, pg_temp;

DO $v1_preflight$
DECLARE
    s OID := pg_catalog.to_regrole('__S_LITERAL__');
    r OID := pg_catalog.to_regrole('__R_LITERAL__');
    a OID := pg_catalog.to_regrole('__A_LITERAL__');
    h OID := pg_catalog.to_regrole('__H_LITERAL__');
    t OID := pg_catalog.to_regrole('__T_LITERAL__');
    i OID := pg_catalog.to_regrole('__I_LITERAL__');
    m OID := pg_catalog.to_regrole('pg_monitor');
    x OID := pg_catalog.to_regrole('__X_LITERAL__');
    epoch OID := pg_catalog.to_regclass('__SCHEMA_PATH__.ingress_protocol_epoch');
    guarded TEXT[] := ARRAY[
        'ingress_messages', 'ingress_origin_aliases', 'ingress_sm_refs',
        'ingress_deliveries', 'ingress_sm_streams', 'ingress_effect_intents',
        'ingress_effect_receipts', 'ingress_carbon_receipts',
        'ingress_delivery_receipts', 'ingress_archive_dispatch',
        'ingress_send_attempts', 'ingress_effect_descendants',
        'ingress_owner_rejections'
    ];
    helper_tables TEXT[] := ARRAY[
        'dnd_projection', 'dnd_projection_source_version',
        'notification_settings_projection', 'notification_settings_projection_source_version',
        'pubsub_affiliations', 'pubsub_items', 'pubsub_nodes', 'pubsub_owner_configured',
        'pubsub_push_publications', 'pubsub_schema_marker', 'pubsub_schema_version',
        'pubsub_subscriptions'
    ];
BEGIN
    IF pg_catalog.current_setting('server_version_num')::INTEGER / 10000 <> 17
       OR pg_catalog.current_setting('transaction_isolation') <> 'read committed'
       OR CURRENT_USER <> SESSION_USER
       OR s IS NULL OR r IS NULL OR a IS NULL OR h IS NULL OR t IS NULL
       OR i IS NULL OR m IS NULL OR x IS NULL
       OR (SELECT pg_catalog.count(DISTINCT role_oid) FROM pg_catalog.unnest(ARRAY[s,r,a,h,t,i,m,x]) role_oid) <> 8
       OR NOT EXISTS (
           SELECT 1 FROM pg_catalog.pg_locks
           WHERE locktype = 'relation' AND relation = epoch
             AND pid = pg_catalog.pg_backend_pid() AND mode = 'AccessExclusiveLock' AND granted
       ) THEN
        RAISE EXCEPTION 'waddle: v1 role graph requires PG17, distinct principals and the epoch barrier';
    END IF;
    IF (SELECT pg_catalog.count(*) FROM pg_catalog.pg_roles role
        WHERE role.oid = ANY(ARRAY[s,r,a,h,t,i,x])
          AND NOT role.rolsuper AND NOT role.rolcreatedb AND NOT role.rolcreaterole
          AND NOT role.rolreplication AND NOT role.rolbypassrls AND NOT role.rolinherit
          AND role.rolcanlogin = (role.oid = ANY(ARRAY[r,i,x]))) <> 7 THEN
        RAISE EXCEPTION 'waddle: v1 role attributes mismatch';
    END IF;
    IF (SELECT pg_catalog.count(*) FROM pg_catalog.pg_auth_members membership
        WHERE (membership.roleid = t AND membership.member = i
            OR membership.roleid = s AND membership.member = x)
          AND membership.set_option AND NOT membership.inherit_option AND NOT membership.admin_option) <> 2
       OR EXISTS (
           SELECT 1 FROM pg_catalog.pg_auth_members membership
           WHERE (membership.roleid = ANY(ARRAY[s,r,a,h,t,i,x])
                  OR membership.member = ANY(ARRAY[s,r,a,h,t,i,x]))
             AND NOT (membership.roleid = t AND membership.member = i
                   OR membership.roleid = s AND membership.member = x)
       ) THEN
        RAISE EXCEPTION 'waddle: v1 role membership mismatch';
    END IF;
    IF pg_catalog.pg_has_role(r, m, 'MEMBER')
       OR pg_catalog.has_database_privilege(r, pg_catalog.current_database(), 'TEMP')
       OR pg_catalog.has_parameter_privilege(r, 'session_replication_role', 'SET')
       OR pg_catalog.has_schema_privilege(r, '__SCHEMA_LITERAL__', 'CREATE')
       OR pg_catalog.has_schema_privilege(r, '__TRUSTED_SCHEMA_LITERAL__', 'CREATE')
       OR EXISTS (
           SELECT 1 FROM pg_catalog.pg_proc p
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
           WHERE n.nspname = 'pg_catalog'
             AND (pg_catalog.starts_with(p.proname, 'pg_ls_')
                  OR p.proname = ANY(ARRAY['pg_read_file','pg_read_binary_file',
                                           'pg_stat_file','lo_import','lo_export']))
             AND pg_catalog.has_function_privilege(r, p.oid, 'EXECUTE')
       ) THEN
        RAISE EXCEPTION 'waddle: v1 runtime has creation, monitor, replication-role, or server-file authority';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n
                   WHERE n.nspname = '__SCHEMA_LITERAL__' AND n.nspowner = s)
       OR (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = '__SCHEMA_LITERAL__'
             AND c.relname = ANY(guarded || helper_tables || ARRAY[
                 'ingress_protocol_epoch','ingress_epoch_guard_manifest',
                 'p21_receipt_operation_results','sessions','pending_delivery'])
             AND c.relkind = 'r' AND c.relpersistence = 'p' AND c.relowner = s
             AND NOT c.relispartition AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh
                             WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid)) <> 30 THEN
        RAISE EXCEPTION 'waddle: v1 relation inventory mismatch';
    END IF;
    -- A/H begin without ownership, memberships, table or column grants in this
    -- database. Other stores must supply their own explicit, reviewed matrix.
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.relowner = ANY(ARRAY[a,h]))
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc p WHERE p.proowner = ANY(ARRAY[a,h]))
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_class c
                  CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) acl
                  WHERE acl.grantee = ANY(ARRAY[a,h]))
       OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute att
                  CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl
                  WHERE acl.grantee = ANY(ARRAY[a,h])) THEN
        RAISE EXCEPTION 'waddle: v1 API/helper role already has object authority';
    END IF;
    -- On guarded relations allow only owner, removable R/PUBLIC legacy grants,
    -- and non-grantable M SELECT. Any third-party relay fails closed.
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
        WHERE n.nspname = '__SCHEMA_LITERAL__'
          AND c.relname = ANY(guarded || ARRAY['ingress_protocol_epoch','ingress_epoch_guard_manifest',
                                              'p21_receipt_operation_results'])
          AND acl.grantee <> s AND NOT (
              acl.grantee IN (0,r) AND NOT acl.is_grantable
              OR acl.grantee = m AND acl.privilege_type = 'SELECT' AND NOT acl.is_grantable)
    ) OR EXISTS (
        SELECT 1 FROM pg_catalog.pg_class c
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_catalog.pg_attribute att ON att.attrelid = c.oid
        CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl
        WHERE n.nspname = '__SCHEMA_LITERAL__'
          AND c.relname = ANY(guarded || ARRAY['ingress_protocol_epoch','ingress_epoch_guard_manifest',
                                              'p21_receipt_operation_results'])
          AND acl.grantee <> s AND NOT (
              c.relname = 'ingress_protocol_epoch' AND att.attname = 'id'
              AND acl.grantee = r AND acl.privilege_type = 'UPDATE' AND NOT acl.is_grantable)
    ) THEN
        RAISE EXCEPTION 'waddle: v1 unexpected guarded relation ACL';
    END IF;
    IF (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = '__SCHEMA_LITERAL__' AND p.proname = 'pubsub_runtime_attach_lock'
          AND p.pronargs = 0 AND p.proowner = s) <> 1
       OR (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
           WHERE n.nspname = '__SCHEMA_LITERAL__' AND p.proname = 'pubsub_runtime_attach_lock') <> 1
       OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
           WHERE n.nspname = '__SCHEMA_LITERAL__'
             AND p.oid = pg_catalog.to_regprocedure('__SCHEMA_PATH__.waddle_p21_record_effect_receipt(uuid,uuid,integer,bytea)')
             AND p.proowner = s AND NOT p.prosecdef)
       OR EXISTS (SELECT 1 FROM __SCHEMA__.p21_receipt_operation_results) THEN
        RAISE EXCEPTION 'waddle: v1 legacy helper or empty journal prerequisite failed';
    END IF;
END
$v1_preflight$;

DO $v1_grants$
DECLARE
    table_name TEXT;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'ingress_messages', 'ingress_origin_aliases', 'ingress_sm_refs',
        'ingress_deliveries', 'ingress_sm_streams', 'ingress_effect_intents',
        'ingress_effect_receipts', 'ingress_carbon_receipts',
        'ingress_delivery_receipts', 'ingress_archive_dispatch',
        'ingress_send_attempts', 'ingress_effect_descendants',
        'ingress_owner_rejections'
    ] LOOP
        EXECUTE pg_catalog.format('REVOKE ALL ON TABLE %I.%I FROM PUBLIC, %I', '__SCHEMA_LITERAL__', table_name, '__R_LITERAL__');
        EXECUTE pg_catalog.format('GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE %I.%I TO %I', '__SCHEMA_LITERAL__', table_name, '__A_LITERAL__');
    END LOOP;
    FOREACH table_name IN ARRAY ARRAY[
        'dnd_projection', 'dnd_projection_source_version',
        'notification_settings_projection', 'notification_settings_projection_source_version',
        'pubsub_affiliations', 'pubsub_items', 'pubsub_nodes', 'pubsub_owner_configured',
        'pubsub_push_publications', 'pubsub_schema_marker', 'pubsub_schema_version',
        'pubsub_subscriptions'
    ] LOOP
        -- UPDATE is the least table privilege that permits SHARE UPDATE
        -- EXCLUSIVE; H has no login or memberships and owns one fixed helper.
        EXECUTE pg_catalog.format('GRANT UPDATE ON TABLE %I.%I TO %I', '__SCHEMA_LITERAL__', table_name, '__H_LITERAL__');
    END LOOP;
END
$v1_grants$;

REVOKE ALL ON __SCHEMA__.ingress_protocol_epoch, __SCHEMA__.ingress_epoch_guard_manifest,
    __SCHEMA__.p21_receipt_operation_results FROM PUBLIC, __R_IDENT__;
REVOKE UPDATE (id) ON __SCHEMA__.ingress_protocol_epoch FROM __R_IDENT__;
GRANT SELECT ON __SCHEMA__.ingress_protocol_epoch TO __R_IDENT__, __A_IDENT__;
GRANT UPDATE (id) ON __SCHEMA__.ingress_protocol_epoch TO __A_IDENT__;
GRANT SELECT ON __SCHEMA__.ingress_epoch_guard_manifest TO __R_IDENT__;
GRANT SELECT ON __SCHEMA__.sessions, __SCHEMA__.pending_delivery TO __A_IDENT__;
GRANT UPDATE (id) ON __SCHEMA__.sessions TO __A_IDENT__;
GRANT SELECT, INSERT ON __SCHEMA__.p21_receipt_operation_results TO __A_IDENT__;
GRANT USAGE ON SCHEMA __SCHEMA__ TO __R_IDENT__, __A_IDENT__, __H_IDENT__;
-- PostgreSQL requires prospective function owners to have CREATE. Revoke it
-- before commit; only this trusted, fenced transition can observe it.
GRANT CREATE ON SCHEMA __SCHEMA__ TO __A_IDENT__, __H_IDENT__;

__FIXED_INGRESS_API__

-- The retained statement trigger is invoker-only and belongs to S; all fixed
-- operation functions are transferred to A by the pinned API asset.
ALTER FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard() OWNER TO __S_IDENT__;
ALTER FUNCTION __SCHEMA__.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA) OWNER TO __A_IDENT__;
ALTER FUNCTION __SCHEMA__.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA) SECURITY DEFINER;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA) TO __R_IDENT__;
-- R reaches effect receipts only through the fixed operation-ID journal API.
SET LOCAL ROLE __A_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_record_effect_receipt(UUID, INTEGER, BYTEA)
    FROM PUBLIC, __R_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_record_policy_discard(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC, __R_IDENT__;
__FIXED_RECEIPT_LOOKUP__
RESET ROLE;

-- Drop the S-owned callable object rather than keeping a second authority.
-- Existing dependents make DROP fail; no CASCADE is permitted.
DROP FUNCTION __SCHEMA__.pubsub_runtime_attach_lock();
CREATE FUNCTION __SCHEMA__.pubsub_runtime_attach_lock() RETURNS VOID
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $v1_pubsub_lock$
    LOCK TABLE ONLY __SCHEMA__.dnd_projection IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.dnd_projection_source_version IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.notification_settings_projection IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.notification_settings_projection_source_version IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_affiliations IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_items IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_nodes IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_owner_configured IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_push_publications IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_schema_marker IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_schema_version IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    LOCK TABLE ONLY __SCHEMA__.pubsub_subscriptions IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;
    SELECT NULL::pg_catalog.void;
$v1_pubsub_lock$;
ALTER FUNCTION __SCHEMA__.pubsub_runtime_attach_lock() OWNER TO __H_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.pubsub_runtime_attach_lock() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.pubsub_runtime_attach_lock() TO __R_IDENT__;
REVOKE CREATE ON SCHEMA __SCHEMA__ FROM __A_IDENT__, __H_IDENT__;

DO $v1_postflight$
DECLARE
    r OID := pg_catalog.to_regrole('__R_LITERAL__');
    a OID := pg_catalog.to_regrole('__A_LITERAL__');
    h OID := pg_catalog.to_regrole('__H_LITERAL__');
BEGIN
    IF pg_catalog.has_schema_privilege(a, '__SCHEMA_LITERAL__', 'CREATE')
       OR pg_catalog.has_schema_privilege(h, '__SCHEMA_LITERAL__', 'CREATE')
       OR pg_catalog.has_table_privilege(r, '__SCHEMA_PATH__.ingress_protocol_epoch', 'UPDATE')
       OR pg_catalog.has_column_privilege(r, '__SCHEMA_PATH__.ingress_protocol_epoch', 'id', 'UPDATE')
       OR NOT pg_catalog.has_table_privilege(r, '__SCHEMA_PATH__.ingress_protocol_epoch', 'SELECT')
       OR NOT pg_catalog.has_table_privilege(a, '__SCHEMA_PATH__.ingress_protocol_epoch', 'SELECT')
       OR NOT pg_catalog.has_column_privilege(a, '__SCHEMA_PATH__.ingress_protocol_epoch', 'id', 'UPDATE')
       OR pg_catalog.has_column_privilege(a, '__SCHEMA_PATH__.ingress_protocol_epoch', 'epoch', 'UPDATE')
       OR pg_catalog.has_table_privilege(a, '__SCHEMA_PATH__.ingress_protocol_epoch', 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR pg_catalog.has_table_privilege(r, '__SCHEMA_PATH__.p21_receipt_operation_results', 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR pg_catalog.has_any_column_privilege(r, '__SCHEMA_PATH__.p21_receipt_operation_results', 'SELECT,INSERT,UPDATE,REFERENCES')
       OR NOT pg_catalog.has_table_privilege(a, '__SCHEMA_PATH__.p21_receipt_operation_results', 'SELECT')
       OR NOT pg_catalog.has_table_privilege(a, '__SCHEMA_PATH__.p21_receipt_operation_results', 'INSERT')
       OR pg_catalog.has_table_privilege(a, '__SCHEMA_PATH__.p21_receipt_operation_results', 'UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
       OR (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = '__SCHEMA_LITERAL__' AND c.relname IN ('sessions','pending_delivery')
             AND pg_catalog.has_table_privilege(a, c.oid, 'SELECT')
             AND NOT pg_catalog.has_table_privilege(a, c.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
             AND NOT pg_catalog.has_any_column_privilege(a, c.oid, 'INSERT,REFERENCES')
             AND NOT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_attribute att
                 WHERE att.attrelid = c.oid AND att.attnum <> 0 AND NOT att.attisdropped
                   AND NOT (c.relname = 'sessions' AND att.attname = 'id')
                   AND pg_catalog.has_column_privilege(a, c.oid, att.attnum, 'UPDATE'))
             AND (c.relname <> 'sessions' OR pg_catalog.has_column_privilege(a, c.oid, 'id', 'UPDATE'))) <> 2
       OR (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = '__SCHEMA_LITERAL__' AND c.relname = ANY(ARRAY[
               'ingress_messages', 'ingress_origin_aliases', 'ingress_sm_refs',
               'ingress_deliveries', 'ingress_sm_streams', 'ingress_effect_intents',
               'ingress_effect_receipts', 'ingress_carbon_receipts',
               'ingress_delivery_receipts', 'ingress_archive_dispatch',
               'ingress_send_attempts', 'ingress_effect_descendants',
               'ingress_owner_rejections'])
             AND NOT pg_catalog.has_table_privilege(r, c.oid, 'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
             AND NOT pg_catalog.has_any_column_privilege(r, c.oid, 'SELECT,INSERT,UPDATE,REFERENCES')
             AND pg_catalog.has_table_privilege(a, c.oid, 'SELECT')
             AND pg_catalog.has_table_privilege(a, c.oid, 'INSERT')
             AND pg_catalog.has_table_privilege(a, c.oid, 'UPDATE')
             AND pg_catalog.has_table_privilege(a, c.oid, 'DELETE')
             AND NOT pg_catalog.has_table_privilege(a, c.oid, 'TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
             AND NOT pg_catalog.has_table_privilege(a, c.oid, 'UPDATE WITH GRANT OPTION')) <> 13
       OR (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = '__SCHEMA_LITERAL__' AND c.relname = ANY(ARRAY[
               'dnd_projection', 'dnd_projection_source_version',
               'notification_settings_projection', 'notification_settings_projection_source_version',
               'pubsub_affiliations', 'pubsub_items', 'pubsub_nodes', 'pubsub_owner_configured',
               'pubsub_push_publications', 'pubsub_schema_marker', 'pubsub_schema_version',
               'pubsub_subscriptions'])
             AND pg_catalog.has_table_privilege(h, c.oid, 'UPDATE')
             AND NOT pg_catalog.has_table_privilege(h, c.oid, 'SELECT,INSERT,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
             AND NOT pg_catalog.has_any_column_privilege(h, c.oid, 'SELECT,INSERT,REFERENCES')
             AND NOT pg_catalog.has_table_privilege(h, c.oid, 'UPDATE WITH GRANT OPTION')) <> 12
       OR EXISTS (
           SELECT 1 FROM pg_catalog.pg_proc p
           CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl, pg_catalog.acldefault('f',p.proowner))) acl
           WHERE p.proowner = ANY(ARRAY[a,h]) AND acl.grantee <> p.proowner
             AND (acl.grantee <> r OR acl.privilege_type <> 'EXECUTE' OR acl.is_grantable)
       )
       OR pg_catalog.has_function_privilege(r, '__SCHEMA_PATH__.waddle_ingress_definer_prepare()', 'EXECUTE')
       OR pg_catalog.has_function_privilege(r, '__SCHEMA_PATH__.waddle_ingress_definer_epoch_guard()', 'EXECUTE')
       OR pg_catalog.has_function_privilege(r, '__SCHEMA_PATH__.waddle_ingress_record_effect_receipt(uuid,integer,bytea)', 'EXECUTE')
       OR pg_catalog.has_function_privilege(r, '__SCHEMA_PATH__.waddle_ingress_record_policy_discard(uuid,integer,bytea,text)', 'EXECUTE')
       OR (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc WHERE proowner = h) <> 1 THEN
        RAISE EXCEPTION 'waddle: v1 role graph postflight failed';
    END IF;
END
$v1_postflight$;
