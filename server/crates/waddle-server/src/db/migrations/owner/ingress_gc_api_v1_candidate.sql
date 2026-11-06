-- DARK, QUIESCENT-WRITER LABORATORY ONLY. This operation is not registered,
-- called by startup, or safe for production. A detached stream can append an
-- already-authorized SM frame after the canonical lock/check; the caller must
-- keep all such writers stopped from retention adoption through collection.
-- Install atomically with the v1 FK and restricted-runtime API transition.
-- The caller must prelock and physically attest the expected epoch relation on
-- this same READ COMMITTED transaction before invoking the function.
GRANT SELECT, DELETE ON __SCHEMA__.muc_invite_claims TO __API_OWNER_IDENT__;
GRANT SELECT, DELETE ON __SCHEMA__.sm_ingress_appends TO __API_OWNER_IDENT__;

CREATE FUNCTION __SCHEMA__.waddle_ingress_gc_candidate_v1(p_key UUID)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
-- Do not bypass RLS. PostgreSQL errors if any referenced relation would
-- filter rows, rather than letting hidden custody look absent to this API.
SET row_security = off
AS $$
DECLARE
    live_epoch BIGINT;
    db_now TIMESTAMPTZ := pg_catalog.clock_timestamp();
    source_decision SMALLINT;
    child_key UUID;
    member UUID;
    locked_key UUID;
    ready BOOLEAN;
    expired BOOLEAN;
    all_ready BOOLEAN := true;
    adopted BOOLEAN := false;
    pending_delivery BOOLEAN;
    affected BIGINT;
BEGIN
    IF p_key IS NULL THEN
        RAISE EXCEPTION 'waddle gc v1: null candidate';
    END IF;
    live_epoch := __SCHEMA__.waddle_ingress_definer_prepare();
    IF live_epoch > 1 THEN
        RAISE EXCEPTION 'waddle gc v1: unsupported protocol epoch';
    END IF;
    -- The v1 pilot cannot interpret either experimental admission record.
    -- Recheck after preparation even if installation checked the schema.
    -- As with the FK manifest, privileged DDL must remain quiescent.
    IF pg_catalog.to_regclass('__SCHEMA_PATH__.ingress_admission_pins') IS NOT NULL
       OR pg_catalog.to_regclass('__SCHEMA_PATH__.direct_admission_lab') IS NOT NULL THEN
        RAISE EXCEPTION 'waddle gc v1: unsupported admission custody relation';
    END IF;

    -- Match the collector's source-then-child order and never wait on a
    -- competing canonical-row owner. The caller bounds all other lock waits.
    SELECT m.message_key, m.owner_decision INTO locked_key, source_decision
    FROM __SCHEMA__.ingress_messages m
    WHERE m.message_key = p_key AND m.terminal_at IS NOT NULL
      AND (m.retention_eligible_at IS NULL OR m.retention_eligible_at <= db_now - INTERVAL '8 days')
    FOR UPDATE SKIP LOCKED;
    IF locked_key IS NULL THEN RETURN 'skipped'; END IF;
    IF EXISTS (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections r WHERE r.child_message_key = p_key) THEN
        RETURN 'retained';
    END IF;
    SELECT r.child_message_key INTO child_key
    FROM __SCHEMA__.ingress_owner_rejections r WHERE r.source_message_key = p_key;
    IF (child_key IS NULL AND source_decision = 2)
       OR (child_key IS NOT NULL AND source_decision IS DISTINCT FROM 2) THEN
        RETURN 'retained';
    END IF;
    IF child_key IS NOT NULL THEN
        locked_key := NULL;
        SELECT m.message_key INTO locked_key FROM __SCHEMA__.ingress_messages m
        WHERE m.message_key = child_key AND m.terminal_at IS NOT NULL
          AND (m.retention_eligible_at IS NULL OR m.retention_eligible_at <= db_now - INTERVAL '8 days')
        FOR UPDATE SKIP LOCKED;
        IF locked_key IS NULL THEN RETURN 'skipped'; END IF;
        IF EXISTS (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections r WHERE r.source_message_key = child_key)
           OR EXISTS (SELECT 1 FROM __SCHEMA__.ingress_messages m WHERE m.message_key = child_key AND m.owner_decision = 2) THEN
            RETURN 'retained';
        END IF;
    END IF;

    FOREACH member IN ARRAY ARRAY[p_key, child_key] LOOP
        IF member IS NULL THEN CONTINUE; END IF;
        pending_delivery := false;
        IF pg_catalog.to_regclass('__SCHEMA_PATH__.pending_delivery') IS NOT NULL THEN
            SELECT EXISTS (
                SELECT 1 FROM __SCHEMA__.ingress_archive_dispatch d
                JOIN __SCHEMA__.pending_delivery p ON p.row_id = d.pending_row_id
                WHERE d.message_key = member AND d.pending_row_id <> ''
            ) INTO pending_delivery;
        END IF;
        SELECT m.terminal_at IS NOT NULL
          AND NOT pending_delivery
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_sm_refs r WHERE r.message_key = member)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.sm_ingress_appends a WHERE a.message_key = member::text AND a.disposition = 0)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_descendants d WHERE d.message_key = member AND d.settled_at IS NULL)
          AND NOT EXISTS (
              SELECT 1 FROM __SCHEMA__.ingress_effect_intents i WHERE i.message_key = member
                AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_receipts r
                  WHERE r.message_key = i.message_key AND r.kind = i.kind
                    AND r.semantic_identity_hash = i.semantic_identity_hash))
          AND NOT (m.source_binding_version IS NOT NULL AND (m.owner_decision IS NULL OR m.owner_decision = 0)
              AND EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_intents i WHERE i.message_key = member AND i.kind = 12)),
          m.retention_eligible_at IS NOT NULL AND m.retention_eligible_at <= db_now - INTERVAL '8 days'
        INTO ready, expired FROM __SCHEMA__.ingress_messages m WHERE m.message_key = member;
        IF NOT ready THEN
            UPDATE __SCHEMA__.ingress_messages SET retention_eligible_at = NULL WHERE message_key = member;
            all_ready := false;
        ELSIF NOT expired THEN
            UPDATE __SCHEMA__.ingress_messages SET retention_eligible_at = COALESCE(retention_eligible_at, db_now)
            WHERE message_key = member;
            all_ready := false;
            adopted := true;
        END IF;
    END LOOP;
    IF NOT all_ready THEN
        IF adopted THEN RETURN 'adopted'; END IF;
        RETURN 'retained';
    END IF;

    -- All the guarded cascade edges are NO ACTION. Delete exactly the leaves
    -- first while the source and child locks are held; keep the unguarded MUC
    -- FK in the manifest and delete its rows explicitly too.
    FOREACH member IN ARRAY ARRAY[child_key, p_key] LOOP
        IF member IS NULL THEN CONTINUE; END IF;
        DELETE FROM __SCHEMA__.ingress_archive_dispatch WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_carbon_receipts WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_delivery_receipts WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_effect_descendants WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_effect_receipts WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_send_attempts WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_effect_intents WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_origin_aliases WHERE message_key = member;
        DELETE FROM __SCHEMA__.ingress_deliveries WHERE message_key = member;
        DELETE FROM __SCHEMA__.muc_invite_claims WHERE message_key = member;
    END LOOP;

    IF child_key IS NOT NULL THEN
        SET CONSTRAINTS __SCHEMA__.waddle_ingress_owner_rejections_child_gc_v1_fkey DEFERRED;
        DELETE FROM __SCHEMA__.ingress_messages WHERE message_key = child_key;
        GET DIAGNOSTICS affected = ROW_COUNT;
        IF affected <> 1 THEN RAISE EXCEPTION 'waddle gc v1: child disappeared'; END IF;
        DELETE FROM __SCHEMA__.ingress_owner_rejections
        WHERE source_message_key = p_key AND child_message_key = child_key;
        GET DIAGNOSTICS affected = ROW_COUNT;
        IF affected <> 1 THEN RAISE EXCEPTION 'waddle gc v1: mapping disappeared'; END IF;
    END IF;
    DELETE FROM __SCHEMA__.ingress_messages WHERE message_key = p_key;
    GET DIAGNOSTICS affected = ROW_COUNT;
    IF affected <> 1 THEN RAISE EXCEPTION 'waddle gc v1: source disappeared'; END IF;
    IF child_key IS NOT NULL THEN
        SET CONSTRAINTS __SCHEMA__.waddle_ingress_owner_rejections_child_gc_v1_fkey IMMEDIATE;
    END IF;
    DELETE FROM __SCHEMA__.sm_ingress_appends
    WHERE message_key IN (p_key::text, child_key::text) AND disposition <> 0;
    IF child_key IS NOT NULL THEN RETURN 'deleted_pair'; END IF;
    RETURN 'deleted_one';
END
$$;
ALTER FUNCTION __SCHEMA__.waddle_ingress_gc_candidate_v1(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_gc_candidate_v1(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_gc_candidate_v1(UUID) TO __RUNTIME_ROLE_IDENT__;
