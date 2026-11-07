-- UNREGISTERED, DARK candidate. The trusted provisioner renders identifiers
-- and runs this after the v1 role graph, independently proving its object
-- identities and fencing old logins, backends, prepared transactions and
-- nonparticipating catalog/role mutators. This is not a serving transition.
-- The singleton tables must already be owner-prepared.
SET LOCAL search_path = pg_catalog, pg_temp;

REVOKE ALL ON __SCHEMA__.singleton_candidate_slot,
    __SCHEMA__.singleton_candidate_leases FROM PUBLIC, __RUNTIME_ROLE_IDENT__, __API_OWNER_IDENT__;
REVOKE ALL (slot, node_id, incarnation, generation)
    ON __SCHEMA__.singleton_candidate_slot FROM PUBLIC, __RUNTIME_ROLE_IDENT__, __API_OWNER_IDENT__;
REVOKE ALL (incarnation, node_id, generation, expires_at, revoked)
    ON __SCHEMA__.singleton_candidate_leases FROM PUBLIC, __RUNTIME_ROLE_IDENT__, __API_OWNER_IDENT__;
GRANT SELECT ON __SCHEMA__.singleton_candidate_slot, __SCHEMA__.singleton_candidate_leases
    TO __API_OWNER_IDENT__;
-- FOR SHARE requires UPDATE on at least one column. A owns only the fixed
-- reviewed API and cannot be assumed by R; R receives no row-lock privilege.
GRANT UPDATE (slot) ON __SCHEMA__.singleton_candidate_slot TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA)
    FROM PUBLIC, __RUNTIME_ROLE_IDENT__;
-- The v1 graph may retain monitor SELECT; the journal contains receipt
-- metadata and this narrower candidate does not grant it to pg_monitor.
REVOKE SELECT ON __SCHEMA__.p21_receipt_operation_results FROM pg_monitor;
REVOKE SELECT (operation_id, message_key, receipt_kind, semantic_identity_hash, inserted)
    ON __SCHEMA__.p21_receipt_operation_results FROM pg_monitor;

CREATE FUNCTION __SCHEMA__.waddle_p21_guarded_record_effect_receipt(
    p_node_id TEXT, p_incarnation UUID, p_generation BIGINT,
    p_operation_id UUID, p_message_key UUID, p_receipt_kind INTEGER,
    p_semantic_identity_hash BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $p21_guarded$
DECLARE
    slot_node TEXT;
    slot_incarnation UUID;
    slot_generation BIGINT;
    lease_node TEXT;
    lease_generation BIGINT;
    lease_expires TIMESTAMPTZ;
    lease_revoked BOOLEAN;
BEGIN
    IF p_node_id IS NULL OR pg_catalog.octet_length(p_node_id) NOT BETWEEN 1 AND 255
       OR p_incarnation IS NULL OR p_incarnation = '00000000-0000-0000-0000-000000000000'::UUID
       OR p_generation IS NULL OR p_generation <= 0 THEN
        RAISE EXCEPTION 'waddle: malformed singleton grant' USING ERRCODE = '22023';
    END IF;
    -- This verifies session and API-owner identities, READ COMMITTED, the
    -- catalog fence, and the caller's earlier epoch relation prelock.
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    SELECT node_id, incarnation, generation
      INTO STRICT slot_node, slot_incarnation, slot_generation
      FROM ONLY __SCHEMA__.singleton_candidate_slot
     WHERE slot = 1 FOR SHARE NOWAIT;
    IF slot_node IS DISTINCT FROM p_node_id OR slot_incarnation IS DISTINCT FROM p_incarnation
       OR slot_generation IS DISTINCT FROM p_generation THEN
        RAISE EXCEPTION 'waddle: singleton grant is not current' USING ERRCODE = '55000';
    END IF;
    -- Registration and revocation hold slot FOR UPDATE before changing this
    -- row. Heartbeats lock only the lease, so do not take a lease row lock.
    SELECT node_id, generation, expires_at, revoked
      INTO STRICT lease_node, lease_generation, lease_expires, lease_revoked
      FROM ONLY __SCHEMA__.singleton_candidate_leases
     WHERE incarnation = p_incarnation;
    IF lease_node IS DISTINCT FROM p_node_id OR lease_generation IS DISTINCT FROM p_generation
       OR lease_revoked IS DISTINCT FROM FALSE OR lease_expires IS NULL
       OR lease_expires <= pg_catalog.clock_timestamp() THEN
        RAISE EXCEPTION 'waddle: singleton lease is not live' USING ERRCODE = '55000';
    END IF;
    RETURN __SCHEMA__.waddle_p21_record_effect_receipt(
        p_operation_id, p_message_key, p_receipt_kind, p_semantic_identity_hash
    );
END
$p21_guarded$;

-- Remove default grants, including unexpected non-PUBLIC default grantees.
DO $p21_guarded_acl$
DECLARE target RECORD;
BEGIN
    FOR target IN
        SELECT DISTINCT acl.grantee, role.rolname
        FROM pg_catalog.pg_proc p
        CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) acl
        LEFT JOIN pg_catalog.pg_roles role ON role.oid = acl.grantee
        WHERE p.oid = '__SCHEMA_PATH__.waddle_p21_guarded_record_effect_receipt(text,uuid,bigint,uuid,uuid,integer,bytea)'::pg_catalog.regprocedure
          AND acl.grantee <> p.proowner
    LOOP
        IF target.grantee = 0 THEN
            REVOKE ALL ON FUNCTION __SCHEMA__.waddle_p21_guarded_record_effect_receipt(TEXT,UUID,BIGINT,UUID,UUID,INTEGER,BYTEA) FROM PUBLIC;
        ELSE
            EXECUTE pg_catalog.format('REVOKE ALL ON FUNCTION %s FROM %I',
                '__SCHEMA_PATH__.waddle_p21_guarded_record_effect_receipt(text,uuid,bigint,uuid,uuid,integer,bytea)',
                target.rolname);
        END IF;
    END LOOP;
END
$p21_guarded_acl$;
GRANT CREATE ON SCHEMA __SCHEMA__ TO __API_OWNER_IDENT__;
ALTER FUNCTION __SCHEMA__.waddle_p21_guarded_record_effect_receipt(TEXT,UUID,BIGINT,UUID,UUID,INTEGER,BYTEA)
    OWNER TO __API_OWNER_IDENT__;
REVOKE CREATE ON SCHEMA __SCHEMA__ FROM __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_p21_guarded_record_effect_receipt(TEXT,UUID,BIGINT,UUID,UUID,INTEGER,BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_p21_guarded_record_effect_receipt(TEXT,UUID,BIGINT,UUID,UUID,INTEGER,BYTEA)
    TO __RUNTIME_ROLE_IDENT__;
