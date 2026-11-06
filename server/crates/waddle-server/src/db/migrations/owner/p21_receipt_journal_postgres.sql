-- The receipt journal is inert until an owner-controlled, separately fenced
-- transition transfers this function to the API owner, changes it to SECURITY
-- DEFINER, and grants only the documented journal and EXECUTE privileges.
-- Installing this migration neither changes legacy writers nor grants runtime
-- access. The wrapper body is checked before each selected transaction.
CREATE TABLE p21_receipt_operation_results (
    operation_id UUID PRIMARY KEY,
    message_key UUID NOT NULL,
    receipt_kind INTEGER NOT NULL,
    semantic_identity_hash BYTEA NOT NULL,
    inserted BOOLEAN NOT NULL,
    CONSTRAINT p21_receipt_operation_results_hash_size
        CHECK (pg_catalog.octet_length(semantic_identity_hash) = 32)
);

DO $p21_install$
DECLARE
    target_schema TEXT := pg_catalog.current_schema();
    grant_target RECORD;
BEGIN
    IF target_schema IS NULL THEN
        RAISE EXCEPTION 'waddle: receipt journal requires a current schema';
    END IF;
    -- ALTER DEFAULT PRIVILEGES can otherwise publish or even make the new
    -- journal writable before the separately fenced role transition. The new
    -- table is not visible to other transactions until this migration commits.
    FOR grant_target IN
        SELECT DISTINCT acl.grantee, role.rolname
        FROM pg_catalog.pg_class relation
        JOIN pg_catalog.pg_namespace ns ON ns.oid = relation.relnamespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) acl
        LEFT JOIN pg_catalog.pg_roles role ON role.oid = acl.grantee
        WHERE ns.nspname = target_schema
          AND relation.relname = 'p21_receipt_operation_results'
          AND acl.grantee <> relation.relowner
    LOOP
        IF grant_target.grantee = 0 THEN
            EXECUTE pg_catalog.format('REVOKE ALL ON TABLE %I.p21_receipt_operation_results FROM PUBLIC', target_schema);
        ELSIF grant_target.rolname IS NOT NULL THEN
            EXECUTE pg_catalog.format('REVOKE ALL ON TABLE %I.p21_receipt_operation_results FROM %I', target_schema, grant_target.rolname);
        ELSE
            RAISE EXCEPTION 'waddle: unknown journal default privilege grantee';
        END IF;
    END LOOP;
    EXECUTE pg_catalog.format($p21_function$
CREATE FUNCTION %1$I.waddle_p21_record_effect_receipt(
    p_operation_id UUID,
    p_message_key UUID,
    p_receipt_kind INTEGER,
    p_semantic_identity_hash BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $p21_body$
DECLARE
    prior_message UUID;
    prior_kind INTEGER;
    prior_hash BYTEA;
    prior_inserted BOOLEAN;
    receipt_inserted BOOLEAN;
BEGIN
    IF p_operation_id IS NULL OR p_message_key IS NULL OR p_receipt_kind IS NULL
       OR p_semantic_identity_hash IS NULL
       OR pg_catalog.octet_length(p_semantic_identity_hash) <> 32 THEN
        RAISE EXCEPTION 'waddle: malformed receipt operation' USING ERRCODE = '22023';
    END IF;
    PERFORM %1$I.waddle_ingress_definer_prepare();
    SELECT message_key, receipt_kind, semantic_identity_hash, inserted
      INTO prior_message, prior_kind, prior_hash, prior_inserted
      FROM %1$I.p21_receipt_operation_results
     WHERE operation_id = p_operation_id;
    IF NOT FOUND THEN
        receipt_inserted := %1$I.waddle_ingress_record_effect_receipt(
            p_message_key, p_receipt_kind, p_semantic_identity_hash
        );
        INSERT INTO %1$I.p21_receipt_operation_results
            (operation_id, message_key, receipt_kind, semantic_identity_hash, inserted)
        VALUES
            (p_operation_id, p_message_key, p_receipt_kind, p_semantic_identity_hash, receipt_inserted)
        ON CONFLICT (operation_id) DO NOTHING;
        SELECT message_key, receipt_kind, semantic_identity_hash, inserted
          INTO STRICT prior_message, prior_kind, prior_hash, prior_inserted
          FROM %1$I.p21_receipt_operation_results
         WHERE operation_id = p_operation_id;
    END IF;
    IF prior_message IS DISTINCT FROM p_message_key
       OR prior_kind IS DISTINCT FROM p_receipt_kind
       OR prior_hash IS DISTINCT FROM p_semantic_identity_hash THEN
        RAISE EXCEPTION 'waddle: receipt operation identity conflict' USING ERRCODE = '23505';
    END IF;
    RETURN prior_inserted;
END
$p21_body$
$p21_function$, target_schema);
    -- Functions have PUBLIC EXECUTE by default and may have extra default
    -- grantees. Revoke all of them before the function becomes visible.
    FOR grant_target IN
        SELECT DISTINCT acl.grantee, role.rolname
        FROM pg_catalog.pg_proc proc
        JOIN pg_catalog.pg_namespace ns ON ns.oid = proc.pronamespace
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(proc.proacl, pg_catalog.acldefault('f', proc.proowner))
        ) acl
        LEFT JOIN pg_catalog.pg_roles role ON role.oid = acl.grantee
        WHERE ns.nspname = target_schema
          AND proc.proname = 'waddle_p21_record_effect_receipt'
          AND acl.grantee <> proc.proowner
    LOOP
        IF grant_target.grantee = 0 THEN
            EXECUTE pg_catalog.format('REVOKE ALL ON FUNCTION %I.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA) FROM PUBLIC', target_schema);
        ELSIF grant_target.rolname IS NOT NULL THEN
            EXECUTE pg_catalog.format('REVOKE ALL ON FUNCTION %I.waddle_p21_record_effect_receipt(UUID, UUID, INTEGER, BYTEA) FROM %I', target_schema, grant_target.rolname);
        ELSE
            RAISE EXCEPTION 'waddle: unknown receipt function default privilege grantee';
        END IF;
    END LOOP;
END
$p21_install$;
