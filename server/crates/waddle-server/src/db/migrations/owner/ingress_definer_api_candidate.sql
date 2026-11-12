-- DARK EXPERIMENT ONLY. Render the schema and role placeholders before use.
-- This is deliberately not registered or installed by startup. Its target is
-- a fixed operation-specific API for every table in EPOCH_GUARDED_TABLES,
-- including writes and reads that take row locks. The runtime LOGIN may only
-- execute these functions; the NOLOGIN API owner owns functions but neither
-- tables nor memberships, and the migration identity renders and installs
-- this fragment. Functions must compose inside the caller's existing
-- READ COMMITTED transaction, qualify every relation, and use no dynamic SQL
-- or generic SQL entry point. Do not use this candidate for role or epoch
-- cutover; its installation requires the complete restricted-login ACL,
-- ownership, lock-read denial, and operation test matrix.

-- Keep the registered V1009 trigger and function body intact. PostgreSQL
-- implicitly searches pg_temp first for types when a function's search_path
-- omits it; put pg_temp explicitly last so a hostile temporary `text` domain
-- cannot capture V1009's local TEXT variables during its first invocation.
ALTER FUNCTION __SCHEMA__.waddle_ingress_epoch_guard()
    SET search_path = pg_catalog, pg_temp;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard()
RETURNS trigger
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    live_epoch BIGINT;
BEGIN
    IF CURRENT_USER <> '__API_OWNER_LITERAL__' THEN
        RAISE EXCEPTION 'waddle: canonical ingress writes require the API owner'
            USING ERRCODE = '42501';
    END IF;
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'waddle: canonical ingress writes require READ COMMITTED';
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_locks
        WHERE locktype = 'relation'
          AND relation = pg_catalog.to_regclass('__SCHEMA_PATH__.ingress_protocol_epoch')
          AND pid = pg_catalog.pg_backend_pid()
          AND mode = 'AccessShareLock'
          AND granted
    ) THEN
        RAISE EXCEPTION 'waddle: canonical ingress writes require an epoch ACCESS SHARE prelock';
    END IF;

    LOCK TABLE ONLY __SCHEMA__.ingress_protocol_epoch IN ACCESS SHARE MODE;
    SELECT epoch INTO live_epoch
    FROM __SCHEMA__.ingress_protocol_epoch
    WHERE id = 1;

    IF live_epoch IS NULL
       OR pg_catalog.current_setting('waddle.protocol_epoch', true) IS DISTINCT FROM live_epoch::text
       OR pg_catalog.current_setting('waddle.protocol_epoch_xid', true)
            IS DISTINCT FROM pg_catalog.pg_current_xact_id()::text THEN
        RAISE EXCEPTION 'waddle: canonical ingress write has no current transaction epoch proof';
    END IF;
    RETURN NULL;
END
$$;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_definer_prepare()
RETURNS BIGINT
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    live_epoch BIGINT;
BEGIN
    -- This is the common first-step fence for every runtime-callable ingress
    -- API. It must precede role/catalog/relation inspection and be held through
    -- transaction completion. A false result fails closed; callers retry only
    -- in a fresh READ COMMITTED transaction.
    IF NOT pg_catalog.pg_try_advisory_xact_lock_shared(6841445497037937999) THEN
        RAISE EXCEPTION 'waddle: catalog mutation fence is held'
            USING ERRCODE = '55P03';
    END IF;
    IF SESSION_USER <> '__RUNTIME_ROLE_LITERAL__' THEN
        RAISE EXCEPTION 'waddle: ingress function is restricted to the runtime login'
            USING ERRCODE = '42501';
    END IF;
    IF CURRENT_USER <> '__API_OWNER_LITERAL__' THEN
        RAISE EXCEPTION 'waddle: ingress function requires its dedicated API owner'
            USING ERRCODE = '42501';
    END IF;
    IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'waddle: ingress function requires READ COMMITTED';
    END IF;
    IF NOT EXISTS (
        SELECT 1
        FROM pg_catalog.pg_locks
        WHERE locktype = 'relation'
          AND relation = pg_catalog.to_regclass('__SCHEMA_PATH__.ingress_protocol_epoch')
          AND pid = pg_catalog.pg_backend_pid()
          AND mode = 'AccessShareLock'
          AND granted
    ) THEN
        RAISE EXCEPTION 'waddle: ingress function requires an epoch ACCESS SHARE prelock';
    END IF;

    -- The owning UOW acquired this table lock as its first lock, before any
    -- principal, claim, room, canonical-message, or child-row lock.
    LOCK TABLE ONLY __SCHEMA__.ingress_protocol_epoch IN ACCESS SHARE MODE;
    SELECT epoch INTO live_epoch
    FROM __SCHEMA__.ingress_protocol_epoch
    WHERE id = 1;
    IF live_epoch IS NULL THEN
        RAISE EXCEPTION 'waddle: ingress_protocol_epoch row is missing';
    END IF;
    PERFORM pg_catalog.set_config('waddle.protocol_epoch', live_epoch::TEXT, true);
    PERFORM pg_catalog.set_config(
        'waddle.protocol_epoch_xid', pg_catalog.pg_current_xact_id()::TEXT, true
    );
    RETURN live_epoch;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_definer_prepare() OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_definer_prepare() FROM PUBLIC;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_resolve_origin(
    p_message_key UUID,
    p_digest_version INTEGER,
    p_digest BYTEA,
    p_alias_key_hash BYTEA,
    p_sender_bare_jid TEXT,
    p_target_kind INTEGER,
    p_target_jid TEXT,
    p_origin_id TEXT,
    p_source_bare_jid TEXT,
    p_source_auth_context_id UUID,
    p_source_auth_context_version NUMERIC,
    p_source_auth_epoch NUMERIC,
    p_source_sm_ingress_id UUID,
    p_source_ingress_ordinal NUMERIC
)
RETURNS TABLE (
    canonical_message_key UUID,
    outcome TEXT,
    stored_digest_version INTEGER,
    stored_digest BYTEA
)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    expires_at_text TEXT;
    alias_rows INTEGER;
    message_rows INTEGER;
    stored_key UUID;
    stored_sender TEXT;
    stored_target_kind INTEGER;
    stored_target_jid TEXT;
    stored_origin_id TEXT;
    stored_source_version SMALLINT;
    stored_source_bare_jid TEXT;
    stored_source_context_id UUID;
    stored_source_context_version NUMERIC;
    stored_source_auth_epoch NUMERIC;
    alias_schema TEXT;
    violation_constraint TEXT;
    violation_schema TEXT;
    violation_table TEXT;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    IF p_digest_version IS DISTINCT FROM 1
       OR p_digest IS NULL OR pg_catalog.octet_length(p_digest) <> 32
       OR p_alias_key_hash IS NULL OR pg_catalog.octet_length(p_alias_key_hash) <> 32 THEN
        RAISE EXCEPTION 'waddle: malformed canonical ingress identity'
            USING ERRCODE = '22023';
    END IF;
    IF pg_catalog.num_nonnulls(
           p_source_bare_jid,
           p_source_auth_context_id,
           p_source_auth_context_version,
           p_source_auth_epoch
       ) NOT IN (0, 4)
       OR pg_catalog.num_nonnulls(p_source_sm_ingress_id, p_source_ingress_ordinal) NOT IN (0, 2)
       OR (pg_catalog.num_nonnulls(
               p_source_bare_jid,
               p_source_auth_context_id,
               p_source_auth_context_version,
               p_source_auth_epoch
           ) = 0
           AND pg_catalog.num_nonnulls(p_source_sm_ingress_id, p_source_ingress_ordinal) <> 0) THEN
        RAISE EXCEPTION 'waddle: incomplete canonical source binding'
            USING ERRCODE = '22023';
    END IF;
    IF p_source_bare_jid IS NOT NULL
       AND (p_source_bare_jid <> p_sender_bare_jid
            OR p_source_auth_context_version < 1
            OR p_source_auth_epoch < 1
            OR (p_source_ingress_ordinal IS NOT NULL AND p_source_ingress_ordinal < 1)) THEN
        RETURN QUERY SELECT NULL::UUID, 'source_binding_conflict'::TEXT, NULL::INTEGER, NULL::BYTEA;
        RETURN;
    END IF;

    -- Lock only the canonical parent, preserving the existing GC lock order.
    SELECT a.message_key, m.digest_version, m.digest,
           a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id,
           m.source_binding_version, m.source_bare_jid,
           m.source_auth_context_id, m.source_auth_context_version, m.source_auth_epoch
    INTO stored_key, stored_digest_version, stored_digest,
         stored_sender, stored_target_kind, stored_target_jid, stored_origin_id,
         stored_source_version, stored_source_bare_jid, stored_source_context_id,
         stored_source_context_version, stored_source_auth_epoch
    FROM __SCHEMA__.ingress_origin_aliases AS a
    JOIN __SCHEMA__.ingress_messages AS m USING (message_key)
    WHERE a.alias_key_hash = p_alias_key_hash
    FOR UPDATE OF m;

    IF NOT FOUND THEN
        -- A null candidate key is a read-only probe used to preserve the
        -- caller's lazy key-minting contract. The caller mints only after this
        -- function confirms the alias is absent, then calls again in the same
        -- outer transaction; this function rechecks for a concurrent winner.
        IF p_message_key IS NULL THEN
            RETURN QUERY SELECT NULL::UUID, 'missing'::TEXT, NULL::INTEGER, NULL::BYTEA;
            RETURN;
        END IF;

        -- Assert the durable source principal only when inserting a source
        -- binding. A replay of a legacy alias must remain grandfathered just
        -- as it is in the existing repository path. Keep the session row lock
        -- before any canonical child write, matching principal -> child order.
        IF p_source_bare_jid IS NOT NULL THEN
            SELECT s.expires_at INTO expires_at_text
            FROM __SCHEMA__.sessions AS s
            WHERE s.user_jid = p_source_bare_jid
              AND s.auth_context_id = p_source_auth_context_id::TEXT
              AND s.auth_context_version = p_source_auth_context_version
              AND s.principal_auth_epoch = p_source_auth_epoch
            FOR SHARE OF s;
            IF NOT FOUND OR (
                expires_at_text IS NOT NULL
                AND expires_at_text::TIMESTAMPTZ <= pg_catalog.statement_timestamp()
            ) THEN
                RETURN QUERY SELECT NULL::UUID, 'principal_assertion_failed'::TEXT, NULL::INTEGER, NULL::BYTEA;
                RETURN;
            END IF;
        END IF;
        -- The candidate parent has no children visible to another transaction,
        -- but its alias insert can still lose a concurrent unique-key race.
        -- Keep both tentative rows inside a PL/pgSQL subtransaction so the
        -- unique violation rolls back the parent without an FK cascade. Those
        -- cascades execute child-table triggers as the table owner and are
        -- correctly rejected by the strict candidate guard.
        BEGIN
            INSERT INTO __SCHEMA__.ingress_messages (
                message_key, digest_version, digest,
                source_binding_version, source_bare_jid,
                source_auth_context_id, source_auth_context_version, source_auth_epoch,
                source_sm_ingress_id, source_ingress_ordinal
            ) VALUES (
                p_message_key, p_digest_version, p_digest,
                CASE WHEN p_source_bare_jid IS NULL THEN NULL ELSE 1 END,
                p_source_bare_jid, p_source_auth_context_id,
                p_source_auth_context_version, p_source_auth_epoch,
                p_source_sm_ingress_id, p_source_ingress_ordinal
            )
            ON CONFLICT (message_key) DO NOTHING;
            GET DIAGNOSTICS message_rows = ROW_COUNT;
            IF message_rows = 0 THEN
                RETURN QUERY SELECT NULL::UUID, 'message_key_conflict'::TEXT,
                    NULL::INTEGER, NULL::BYTEA;
                RETURN;
            END IF;

            INSERT INTO __SCHEMA__.ingress_origin_aliases (
                alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key
            ) VALUES (
                p_alias_key_hash, p_sender_bare_jid, p_target_kind, p_target_jid,
                p_origin_id, p_message_key
            );
            GET DIAGNOSTICS alias_rows = ROW_COUNT;
        EXCEPTION
            WHEN unique_violation THEN
                GET STACKED DIAGNOSTICS
                    violation_constraint = CONSTRAINT_NAME,
                    violation_schema = SCHEMA_NAME,
                    violation_table = TABLE_NAME;
                SELECT n.nspname::TEXT
                INTO alias_schema
                FROM pg_catalog.pg_class AS c
                JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace
                WHERE c.oid = pg_catalog.to_regclass('__SCHEMA_PATH__.ingress_origin_aliases');
                IF violation_constraint IS DISTINCT FROM 'ingress_origin_aliases_pkey'
                   OR violation_table IS DISTINCT FROM 'ingress_origin_aliases'
                   OR violation_schema IS DISTINCT FROM alias_schema THEN
                    RAISE;
                END IF;
                alias_rows := 0;
        END;
        IF alias_rows = 1 THEN
            RETURN QUERY SELECT p_message_key, 'inserted'::TEXT, p_digest_version, p_digest;
            RETURN;
        END IF;

        -- A concurrent first writer won the alias. The nested exception block
        -- rolled back the losing candidate; observe the winner under a fresh
        -- READ COMMITTED statement snapshot.
        SELECT a.message_key, m.digest_version, m.digest,
               a.sender_bare_jid, a.target_kind, a.target_jid, a.origin_id,
               m.source_binding_version, m.source_bare_jid,
               m.source_auth_context_id, m.source_auth_context_version, m.source_auth_epoch
        INTO stored_key, stored_digest_version, stored_digest,
             stored_sender, stored_target_kind, stored_target_jid, stored_origin_id,
             stored_source_version, stored_source_bare_jid, stored_source_context_id,
             stored_source_context_version, stored_source_auth_epoch
        FROM __SCHEMA__.ingress_origin_aliases AS a
        JOIN __SCHEMA__.ingress_messages AS m USING (message_key)
        WHERE a.alias_key_hash = p_alias_key_hash
        FOR UPDATE OF m;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'waddle: origin alias disappeared during concurrent resolution';
        END IF;
    END IF;

    IF stored_sender IS DISTINCT FROM p_sender_bare_jid
       OR stored_target_kind IS DISTINCT FROM p_target_kind
       OR stored_target_jid IS DISTINCT FROM p_target_jid
       OR stored_origin_id IS DISTINCT FROM p_origin_id THEN
        RETURN QUERY SELECT stored_key, 'alias_identity_conflict'::TEXT,
            stored_digest_version, stored_digest;
        RETURN;
    END IF;
    IF stored_source_version IS NOT NULL
       AND (p_source_bare_jid IS NULL
            OR stored_source_version <> 1
            OR stored_source_bare_jid IS DISTINCT FROM p_source_bare_jid
            OR stored_source_context_id IS DISTINCT FROM p_source_auth_context_id
            OR stored_source_context_version IS DISTINCT FROM p_source_auth_context_version
            OR stored_source_auth_epoch IS DISTINCT FROM p_source_auth_epoch) THEN
        RETURN QUERY SELECT stored_key, 'source_binding_conflict'::TEXT,
            stored_digest_version, stored_digest;
        RETURN;
    END IF;
    IF stored_digest_version = p_digest_version AND stored_digest = p_digest THEN
        RETURN QUERY SELECT stored_key, 'existing'::TEXT,
            stored_digest_version, stored_digest;
    ELSE
        RETURN QUERY SELECT stored_key, 'digest_conflict'::TEXT,
            stored_digest_version, stored_digest;
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lock_message(p_message_key UUID)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    PERFORM 1
    FROM __SCHEMA__.ingress_messages AS m
    WHERE m.message_key = p_message_key
    FOR UPDATE OF m;
    RETURN FOUND;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lock_message(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lock_message(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lock_message(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_message_is_terminal(p_message_key UUID)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN EXISTS (
        SELECT 1
        FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = p_message_key AND m.terminal_at IS NOT NULL
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_message_is_terminal(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_message_is_terminal(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_message_is_terminal(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_message_created_at(p_message_key UUID)
RETURNS TABLE (created_at TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT pg_catalog.to_char(
        m.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'
    )
    FROM __SCHEMA__.ingress_messages AS m
    WHERE m.message_key = p_message_key;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_message_created_at(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_message_created_at(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_message_created_at(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_load_message_envelope(p_message_key UUID)
RETURNS TABLE (envelope_version INTEGER, envelope BYTEA)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT m.envelope_version::INTEGER, m.envelope
    FROM __SCHEMA__.ingress_messages AS m
    WHERE m.message_key = p_message_key;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_load_message_envelope(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_load_message_envelope(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_load_message_envelope(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_update_message_envelope(
    p_message_key UUID,
    p_envelope_version SMALLINT,
    p_envelope BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_messages AS m
    SET envelope_version = p_envelope_version, envelope = p_envelope
    WHERE m.message_key = p_message_key;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_update_message_envelope(UUID, SMALLINT, BYTEA)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_update_message_envelope(UUID, SMALLINT, BYTEA)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_update_message_envelope(UUID, SMALLINT, BYTEA)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_message(
    p_message_key UUID,
    p_digest_version INTEGER,
    p_digest BYTEA,
    p_envelope_version SMALLINT,
    p_envelope BYTEA,
    p_source_bare_jid TEXT,
    p_source_auth_context_id UUID,
    p_source_auth_context_version NUMERIC,
    p_source_auth_epoch NUMERIC,
    p_source_sm_ingress_id UUID,
    p_source_ingress_ordinal NUMERIC
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_messages (
        message_key, digest_version, digest, envelope_version, envelope,
        source_binding_version, source_bare_jid, source_auth_context_id,
        source_auth_context_version, source_auth_epoch, source_sm_ingress_id,
        source_ingress_ordinal
    ) VALUES (
        p_message_key, p_digest_version, p_digest, p_envelope_version, p_envelope,
        CASE WHEN p_source_bare_jid IS NULL THEN NULL ELSE 1 END,
        p_source_bare_jid, p_source_auth_context_id,
        p_source_auth_context_version, p_source_auth_epoch,
        p_source_sm_ingress_id, p_source_ingress_ordinal
    ) ON CONFLICT (message_key) DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_message(
    UUID, INTEGER, BYTEA, SMALLINT, BYTEA, TEXT, UUID, NUMERIC, NUMERIC, UUID, NUMERIC
) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_message(
    UUID, INTEGER, BYTEA, SMALLINT, BYTEA, TEXT, UUID, NUMERIC, NUMERIC, UUID, NUMERIC
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_message(
    UUID, INTEGER, BYTEA, SMALLINT, BYTEA, TEXT, UUID, NUMERIC, NUMERIC, UUID, NUMERIC
) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_record_delivery(
    p_delivery_key UUID,
    p_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_deliveries (delivery_key, message_key)
    VALUES (p_delivery_key, p_message_key)
    ON CONFLICT (delivery_key) DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_record_delivery(UUID, UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_record_delivery(UUID, UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_record_delivery(UUID, UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lookup_delivery(p_delivery_key UUID)
RETURNS UUID
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN (
        SELECT d.message_key
        FROM __SCHEMA__.ingress_deliveries AS d
        WHERE d.delivery_key = p_delivery_key
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lookup_delivery(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lookup_delivery(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lookup_delivery(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_sm_ref(
    p_sm_ingress_id UUID,
    p_ingress_ordinal NUMERIC,
    p_wire_h BIGINT,
    p_wire_generation BIGINT,
    p_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_sm_refs (
        sm_ingress_id, ingress_ordinal, wire_h, wire_generation, message_key
    ) VALUES (
        p_sm_ingress_id, p_ingress_ordinal, p_wire_h, p_wire_generation, p_message_key
    ) ON CONFLICT (sm_ingress_id, ingress_ordinal) DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_sm_ref(UUID, NUMERIC, BIGINT, BIGINT, UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_sm_ref(UUID, NUMERIC, BIGINT, BIGINT, UUID)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_sm_ref(UUID, NUMERIC, BIGINT, BIGINT, UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream(p_sm_ingress_id UUID)
RETURNS TABLE(
    sm_ingress_id UUID,
    stream_id TEXT,
    handled_ordinal NUMERIC,
    checkpoint_h BIGINT,
    wire_generation BIGINT
)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT s.sm_ingress_id, s.stream_id, s.handled_ordinal, s.checkpoint_h, s.wire_generation
    FROM __SCHEMA__.ingress_sm_streams AS s
    WHERE s.sm_ingress_id = p_sm_ingress_id
    FOR UPDATE OF s;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_delete_sm_ref(
    p_sm_ingress_id UUID,
    p_ingress_ordinal NUMERIC
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    deleted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    DELETE FROM __SCHEMA__.ingress_sm_refs AS r
    WHERE r.sm_ingress_id = p_sm_ingress_id AND r.ingress_ordinal = p_ingress_ordinal;
    GET DIAGNOSTICS deleted_rows = ROW_COUNT;
    RETURN deleted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_delete_sm_ref(UUID, NUMERIC) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_delete_sm_ref(UUID, NUMERIC) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_delete_sm_ref(UUID, NUMERIC) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_sm_stream(
    p_sm_ingress_id UUID,
    p_stream_id TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_sm_streams (sm_ingress_id, stream_id)
    VALUES (p_sm_ingress_id, p_stream_id)
    ON CONFLICT (stream_id) DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_sm_stream(UUID, TEXT) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_sm_stream(UUID, TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_sm_stream(UUID, TEXT) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_advance_sm_frontier(
    p_sm_ingress_id UUID,
    p_offered_ordinal NUMERIC,
    p_previous_ordinal NUMERIC
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_sm_streams AS s
    SET handled_ordinal = p_offered_ordinal,
        row_revision = s.row_revision + 1,
        updated_at = pg_catalog.now()
    WHERE s.sm_ingress_id = p_sm_ingress_id
      AND s.handled_ordinal = p_previous_ordinal;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_advance_sm_frontier(UUID, NUMERIC, NUMERIC)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_advance_sm_frontier(UUID, NUMERIC, NUMERIC)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_advance_sm_frontier(UUID, NUMERIC, NUMERIC)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_update_sm_checkpoint(
    p_sm_ingress_id UUID,
    p_checkpoint_h BIGINT,
    p_generation_delta BIGINT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_sm_streams AS s
    SET checkpoint_h = p_checkpoint_h,
        wire_generation = s.wire_generation + p_generation_delta
    WHERE s.sm_ingress_id = p_sm_ingress_id;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_update_sm_checkpoint(UUID, BIGINT, BIGINT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_update_sm_checkpoint(UUID, BIGINT, BIGINT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_update_sm_checkpoint(UUID, BIGINT, BIGINT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lookup_sm_stream(TEXT)
RETURNS UUID
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN (
        SELECT s.sm_ingress_id
        FROM __SCHEMA__.ingress_sm_streams AS s
        WHERE s.stream_id = $1
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lookup_sm_stream(TEXT) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lookup_sm_stream(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lookup_sm_stream(TEXT) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream_by_name(p_stream_id TEXT)
RETURNS TABLE(sm_ingress_id UUID, handled_ordinal NUMERIC)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT s.sm_ingress_id, s.handled_ordinal
    FROM __SCHEMA__.ingress_sm_streams AS s
    WHERE s.stream_id = p_stream_id
    FOR UPDATE OF s;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream_by_name(TEXT) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream_by_name(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lock_sm_stream_by_name(TEXT) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_delete_sm_stream(TEXT)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    deleted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    DELETE FROM __SCHEMA__.ingress_sm_streams AS s WHERE s.stream_id = $1;
    GET DIAGNOSTICS deleted_rows = ROW_COUNT;
    RETURN deleted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_delete_sm_stream(TEXT) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_delete_sm_stream(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_delete_sm_stream(TEXT) TO __RUNTIME_ROLE_IDENT__;

-- Descendant custody remains in the caller's transaction. The API locks
-- canonical parents in key order before copying or settling child rows, so a
-- descendant operation composes with the existing GC parent -> child order.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lock_message_nowait(p_message_key UUID)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    PERFORM 1 FROM __SCHEMA__.ingress_messages AS m
    WHERE m.message_key = p_message_key
    FOR UPDATE NOWAIT;
    RETURN FOUND;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lock_message_nowait(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lock_message_nowait(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lock_message_nowait(UUID) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_lock_descendant_ancestors_nowait(
    p_descendant_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    ancestor RECORD;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    FOR ancestor IN
        SELECT DISTINCT d.message_key
        FROM __SCHEMA__.ingress_effect_descendants AS d
        WHERE d.descendant_key = p_descendant_key::TEXT
          AND d.settled_at IS NULL
        ORDER BY d.message_key
    LOOP
        PERFORM 1 FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = ancestor.message_key
        FOR UPDATE NOWAIT;
        IF NOT FOUND THEN
            RETURN FALSE;
        END IF;
    END LOOP;
    RETURN TRUE;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_lock_descendant_ancestors_nowait(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_lock_descendant_ancestors_nowait(UUID)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_lock_descendant_ancestors_nowait(UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_attach_descendant(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_descendant_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_effect_descendants
        (message_key, kind, semantic_identity_hash, descendant_key)
    VALUES
        (p_message_key, p_kind, p_semantic_identity_hash, p_descendant_key::TEXT)
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    IF inserted_rows > 0 THEN
        UPDATE __SCHEMA__.ingress_messages AS m
        SET retention_eligible_at = NULL
        WHERE m.message_key = p_message_key;
    END IF;
    RETURN inserted_rows > 0;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_attach_descendant(UUID, INTEGER, BYTEA, UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_attach_descendant(UUID, INTEGER, BYTEA, UUID)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_attach_descendant(UUID, INTEGER, BYTEA, UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_copy_descendant(
    p_source_descendant UUID,
    p_target_descendant UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    ancestor RECORD;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    FOR ancestor IN
        SELECT DISTINCT d.message_key
        FROM __SCHEMA__.ingress_effect_descendants AS d
        WHERE d.descendant_key = p_source_descendant::TEXT
          AND d.settled_at IS NULL
        ORDER BY d.message_key
    LOOP
        PERFORM 1 FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = ancestor.message_key
        FOR UPDATE NOWAIT;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'waddle: canonical descendant parent is missing'
                USING ERRCODE = '23503';
        END IF;
    END LOOP;

    INSERT INTO __SCHEMA__.ingress_effect_descendants
        (message_key, kind, semantic_identity_hash, descendant_key)
    SELECT d.message_key, d.kind, d.semantic_identity_hash, p_target_descendant::TEXT
    FROM __SCHEMA__.ingress_effect_descendants AS d
    WHERE d.descendant_key = p_source_descendant::TEXT
      AND d.settled_at IS NULL
    ON CONFLICT DO NOTHING;

    UPDATE __SCHEMA__.ingress_messages AS m
    SET retention_eligible_at = NULL
    WHERE m.message_key IN (
        SELECT DISTINCT d.message_key
        FROM __SCHEMA__.ingress_effect_descendants AS d
        WHERE d.descendant_key = p_source_descendant::TEXT
          AND d.settled_at IS NULL
    );
    RETURN TRUE;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_copy_descendant(UUID, UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_copy_descendant(UUID, UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_copy_descendant(UUID, UUID)
    TO __RUNTIME_ROLE_IDENT__;

-- Ancestor fan-out remains unbounded, matching the current Rust operation.
-- This candidate stays inactive until a separately reviewed bounded design
-- preserves the all-parents-before-child lock order without partial settlement.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(
    p_descendant_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    ancestor RECORD;
    ancestor_keys UUID[] := ARRAY[]::UUID[];
    ancestor_key UUID;
    settled_at_value TIMESTAMPTZ;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    FOR ancestor IN
        SELECT DISTINCT d.message_key
        FROM __SCHEMA__.ingress_effect_descendants AS d
        WHERE d.descendant_key = p_descendant_key::TEXT
          AND d.settled_at IS NULL
        ORDER BY d.message_key
    LOOP
        PERFORM 1 FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = ancestor.message_key
        FOR UPDATE NOWAIT;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'waddle: canonical descendant parent is missing'
                USING ERRCODE = '23503';
        END IF;
        ancestor_keys := pg_catalog.array_append(ancestor_keys, ancestor.message_key);
    END LOOP;

    settled_at_value := pg_catalog.clock_timestamp();
    UPDATE __SCHEMA__.ingress_effect_descendants AS d
    SET settled_at = settled_at_value
    WHERE d.descendant_key = p_descendant_key::TEXT
      AND d.settled_at IS NULL;

    FOREACH ancestor_key IN ARRAY ancestor_keys LOOP
        PERFORM __SCHEMA__.waddle_ingress_refresh_retention(ancestor_key);
    END LOOP;
    RETURN TRUE;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(UUID)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(UUID)
    TO __RUNTIME_ROLE_IDENT__;

-- Fixed read surfaces for production SQL which joins outbox/provider rows to
-- canonical effect ancestry. TEXT is intentional: legacy outbox identifiers
-- are opaque and some pre-foundation values are not UUIDs.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_descendant_exists(
    p_descendant_key TEXT,
    p_pending_only BOOLEAN
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    IF p_pending_only IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed descendant lookup request'
            USING ERRCODE = '22023';
    END IF;
    RETURN EXISTS (
        SELECT 1
        FROM __SCHEMA__.ingress_effect_descendants AS d
        WHERE d.descendant_key = p_descendant_key
          AND (NOT p_pending_only OR d.settled_at IS NULL)
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_descendant_exists(TEXT, BOOLEAN)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_descendant_exists(TEXT, BOOLEAN)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_descendant_exists(TEXT, BOOLEAN)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_intent_exists(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN EXISTS (
        SELECT 1
        FROM __SCHEMA__.ingress_effect_intents AS i
        WHERE i.message_key = p_message_key
          AND i.kind = p_kind
          AND i.semantic_identity_hash = p_semantic_identity_hash
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_intent_exists(UUID, INTEGER, BYTEA)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_intent_exists(UUID, INTEGER, BYTEA)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_intent_exists(UUID, INTEGER, BYTEA)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_descendant_settled(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_descendant_key TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN COALESCE((
        SELECT d.settled_at IS NOT NULL
        FROM __SCHEMA__.ingress_effect_descendants AS d
        WHERE d.message_key = p_message_key
          AND d.kind = p_kind
          AND d.semantic_identity_hash = p_semantic_identity_hash
          AND d.descendant_key = p_descendant_key
    ), FALSE);
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_descendant_settled(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_descendant_settled(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_descendant_settled(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_legacy_ancestry_intents(
    p_after_message_key UUID,
    p_after_kind INTEGER,
    p_after_semantic_identity_hash BYTEA,
    p_page_size INTEGER
)
RETURNS TABLE (
    message_key UUID,
    kind INTEGER,
    semantic_identity_hash BYTEA,
    payload_version INTEGER,
    payload BYTEA
)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    IF pg_catalog.num_nonnulls(
           p_after_message_key,
           p_after_kind,
           p_after_semantic_identity_hash
       ) NOT IN (0, 3)
       OR p_page_size IS NULL OR p_page_size < 1 OR p_page_size > 64 THEN
        RAISE EXCEPTION 'waddle: malformed legacy ancestry page request'
            USING ERRCODE = '22023';
    END IF;
    RETURN QUERY
    SELECT i.message_key, i.kind, i.semantic_identity_hash,
           i.payload_version, i.payload
    FROM __SCHEMA__.ingress_effect_intents AS i
    WHERE i.kind IN (7, 21)
      AND (p_after_message_key IS NULL OR
           (i.message_key, i.kind, i.semantic_identity_hash) >
           (p_after_message_key, p_after_kind, p_after_semantic_identity_hash))
    ORDER BY i.message_key, i.kind, i.semantic_identity_hash
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_legacy_ancestry_intents(UUID, INTEGER, BYTEA, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_legacy_ancestry_intents(UUID, INTEGER, BYTEA, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_legacy_ancestry_intents(UUID, INTEGER, BYTEA, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

-- Effect intent operations. Payload bytes remain opaque to SQL; semantic
-- identity and protocol-version validation stay in the Rust codec boundary.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_effect_intent(
    p_message_key UUID,
    p_effect_ordinal NUMERIC,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_payload BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_effect_intents
        (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload)
    VALUES
        (p_message_key, p_effect_ordinal, p_kind, p_semantic_identity_hash, 1, p_payload);
    RETURN TRUE;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_effect_intent(UUID, NUMERIC, INTEGER, BYTEA, BYTEA)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_effect_intent(UUID, NUMERIC, INTEGER, BYTEA, BYTEA)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_effect_intent(UUID, NUMERIC, INTEGER, BYTEA, BYTEA)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_effect_intents(
    p_message_key UUID,
    p_after_ordinal NUMERIC,
    p_page_size INTEGER
)
RETURNS TABLE (
    kind INTEGER,
    semantic_identity_hash BYTEA,
    effect_ordinal TEXT,
    payload_version INTEGER,
    payload BYTEA
)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 THEN
        RAISE EXCEPTION 'waddle: malformed effect intent page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT i.kind, i.semantic_identity_hash, i.effect_ordinal::TEXT,
           i.payload_version, i.payload
    FROM __SCHEMA__.ingress_effect_intents AS i
    WHERE i.message_key = p_message_key
      AND (p_after_ordinal IS NULL OR i.effect_ordinal > p_after_ordinal)
    ORDER BY i.effect_ordinal
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_effect_intents(UUID, NUMERIC, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_effect_intents(UUID, NUMERIC, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_effect_intents(UUID, NUMERIC, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_update_effect_payload(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_payload BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_effect_intents AS i
    SET payload = p_payload
    WHERE i.message_key = p_message_key
      AND i.kind = p_kind
      AND i.semantic_identity_hash = p_semantic_identity_hash
      AND i.payload_version = 1;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_update_effect_payload(UUID, INTEGER, BYTEA, BYTEA)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_update_effect_payload(UUID, INTEGER, BYTEA, BYTEA)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_update_effect_payload(UUID, INTEGER, BYTEA, BYTEA)
    TO __RUNTIME_ROLE_IDENT__;

-- Effect completion receipts are immutable; policy disposition is inserted
-- only when the exact corresponding intent exists and never relabels a prior
-- receipt. Bulk read APIs keep high-fanout recovery operations bounded.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_record_effect_receipt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_effect_receipts
        (message_key, kind, semantic_identity_hash)
    VALUES (p_message_key, p_kind, p_semantic_identity_hash)
    ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_record_effect_receipt(UUID, INTEGER, BYTEA)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_record_effect_receipt(UUID, INTEGER, BYTEA)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_record_effect_receipt(UUID, INTEGER, BYTEA)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_record_policy_discard(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_reason TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_effect_receipts
        (message_key, kind, semantic_identity_hash, policy_discard_reason)
    SELECT i.message_key, i.kind, i.semantic_identity_hash, p_reason
    FROM __SCHEMA__.ingress_effect_intents AS i
    WHERE i.message_key = p_message_key
      AND i.kind = p_kind
      AND i.semantic_identity_hash = p_semantic_identity_hash
    ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_record_policy_discard(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_record_policy_discard(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_record_policy_discard(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_effect_receipt_exists(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_policy_discard_reason TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
        WHERE r.message_key = p_message_key
          AND r.kind = p_kind
          AND r.semantic_identity_hash = p_semantic_identity_hash
          AND (p_policy_discard_reason IS NULL OR
               r.policy_discard_reason = p_policy_discard_reason)
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_effect_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_effect_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_effect_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_effect_receipts(
    p_message_key UUID,
    p_after_kind INTEGER,
    p_after_hash BYTEA,
    p_page_size INTEGER
)
RETURNS TABLE (kind INTEGER, semantic_identity_hash BYTEA)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_kind IS NULL) <> (p_after_hash IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed effect receipt page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT r.kind, r.semantic_identity_hash
    FROM __SCHEMA__.ingress_effect_receipts AS r
    WHERE r.message_key = p_message_key
      AND (p_after_kind IS NULL OR
           (r.kind, r.semantic_identity_hash) > (p_after_kind, p_after_hash))
    ORDER BY r.kind, r.semantic_identity_hash
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_effect_receipts(UUID, INTEGER, BYTEA, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_effect_receipts(UUID, INTEGER, BYTEA, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_effect_receipts(UUID, INTEGER, BYTEA, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_receipts_complete(p_message_key UUID)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN NOT EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS i
        WHERE i.message_key = p_message_key
          AND NOT EXISTS (
              SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
              WHERE r.message_key = i.message_key
                AND r.kind = i.kind
                AND r.semantic_identity_hash = i.semantic_identity_hash)
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_receipts_complete(UUID) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_receipts_complete(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_receipts_complete(UUID) TO __RUNTIME_ROLE_IDENT__;

-- Carbon and delivery progress are per-recipient facts. Fixed APIs retain the
-- exact recipient rows and policy labels required by recovery, without
-- fabricating aggregate receipts.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_carbon_receipt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_recipient TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_carbon_receipts
        (message_key, kind, semantic_identity_hash, recipient)
    VALUES (p_message_key, p_kind, p_semantic_identity_hash, p_recipient)
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_carbon_receipt(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_carbon_receipt(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_carbon_receipt(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_carbon_receipts(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_after_resource TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (recipient TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 THEN
        RAISE EXCEPTION 'waddle: malformed carbon receipt page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT c.recipient FROM __SCHEMA__.ingress_carbon_receipts AS c
    WHERE c.message_key = p_message_key AND c.kind = p_kind
      AND c.semantic_identity_hash = p_semantic_identity_hash
      AND (p_after_resource IS NULL OR c.recipient > p_after_resource)
    ORDER BY c.recipient
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_carbon_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_carbon_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_carbon_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_carbon_receipt_exists(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_carbon_receipts AS c
        WHERE c.message_key = p_message_key AND c.kind = p_kind
          AND c.semantic_identity_hash = p_semantic_identity_hash
          AND c.recipient = p_resource
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_carbon_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_carbon_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_carbon_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_pending_send_attempts(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_after_resource TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (recipient TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 THEN
        RAISE EXCEPTION 'waddle: malformed pending send attempt page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT a.recipient
    FROM __SCHEMA__.ingress_send_attempts AS a
    WHERE a.message_key = p_message_key AND a.kind = p_kind
      AND a.semantic_identity_hash = p_semantic_identity_hash
      AND a.state IN (0, 1)
      AND a.expires_at_ms >
          (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint
      AND (p_after_resource IS NULL OR a.recipient > p_after_resource)
    ORDER BY a.recipient
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_pending_send_attempts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_send_attempts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_send_attempts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_send_attempt_status(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT
)
RETURNS TABLE (state INTEGER, expired INTEGER, recovered INTEGER)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed send attempt identity'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT a.state,
           CASE WHEN a.expires_at_ms <=
               (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint
                THEN 1 ELSE 0 END,
           a.recovered
    FROM __SCHEMA__.ingress_send_attempts AS a
    WHERE a.message_key = p_message_key AND a.kind = p_kind
      AND a.semantic_identity_hash = p_semantic_identity_hash
      AND a.recipient = p_resource;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_send_attempt_status(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_send_attempt_status(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_send_attempt_status(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_next_send_attempt_delay(p_message_key UUID)
RETURNS BIGINT
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    retry_delay BIGINT;
BEGIN
    IF p_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed send attempt message key'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    SELECT MIN(a.expires_at_ms -
        (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint)
    INTO retry_delay
    FROM __SCHEMA__.ingress_send_attempts AS a
    WHERE a.message_key = p_message_key AND a.state IN (0, 1);
    RETURN retry_delay;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_next_send_attempt_delay(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_next_send_attempt_delay(UUID)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_next_send_attempt_delay(UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_claim_send_attempt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_node_id TEXT,
    p_node_incarnation TEXT,
    p_lease_token UUID,
    p_ttl_ms BIGINT,
    p_previously_started INTEGER
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL OR
       p_node_id IS NULL OR p_node_incarnation IS NULL OR p_lease_token IS NULL OR
       p_ttl_ms IS NULL OR p_ttl_ms < 1 OR p_ttl_ms > 9223372036854775807 / 2 OR
       p_previously_started IS NULL OR p_previously_started NOT IN (0, 1) THEN
        RAISE EXCEPTION 'waddle: malformed send attempt claim'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_send_attempts
        (message_key, kind, semantic_identity_hash, recipient, node_id,
         node_incarnation, lease_token, expires_at_ms, state, recovered)
    VALUES (p_message_key, p_kind, p_semantic_identity_hash, p_resource,
            p_node_id, p_node_incarnation, p_lease_token,
            (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint + p_ttl_ms,
            0, p_previously_started)
    ON CONFLICT (message_key, kind, semantic_identity_hash, recipient)
    DO UPDATE SET node_id = EXCLUDED.node_id,
                  node_incarnation = EXCLUDED.node_incarnation,
                  lease_token = EXCLUDED.lease_token,
                  expires_at_ms = EXCLUDED.expires_at_ms,
                  state = 0,
                  recovered = EXCLUDED.recovered;
    RETURN TRUE;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_claim_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID, BIGINT, INTEGER
) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_claim_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID, BIGINT, INTEGER
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_claim_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID, BIGINT, INTEGER
) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_start_send_attempt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_node_id TEXT,
    p_node_incarnation TEXT,
    p_lease_token UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows INTEGER;
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL OR
       p_node_id IS NULL OR p_node_incarnation IS NULL OR p_lease_token IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed send attempt start'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_send_attempts AS a
    SET state = 1,
        expires_at_ms = (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint + 60000
    WHERE a.message_key = p_message_key AND a.kind = p_kind
      AND a.semantic_identity_hash = p_semantic_identity_hash
      AND a.recipient = p_resource AND a.node_id = p_node_id
      AND a.node_incarnation = p_node_incarnation
      AND a.lease_token = p_lease_token AND a.state = 0
      AND a.expires_at_ms > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_start_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID
) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_start_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_start_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID
) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_complete_send_attempt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_node_id TEXT,
    p_node_incarnation TEXT,
    p_lease_token UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows INTEGER;
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL OR
       p_node_id IS NULL OR p_node_incarnation IS NULL OR p_lease_token IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed send attempt completion'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_send_attempts AS a
    SET state = 2
    WHERE a.message_key = p_message_key AND a.kind = p_kind
      AND a.semantic_identity_hash = p_semantic_identity_hash
      AND a.recipient = p_resource AND a.node_id = p_node_id
      AND a.node_incarnation = p_node_incarnation
      AND a.lease_token = p_lease_token AND a.state = 1;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_complete_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID
) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_complete_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_complete_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID
) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_release_send_attempt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_node_id TEXT,
    p_node_incarnation TEXT,
    p_lease_token UUID,
    p_recovered INTEGER,
    p_previously_started INTEGER
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    changed_rows INTEGER;
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL OR
       p_node_id IS NULL OR p_node_incarnation IS NULL OR p_lease_token IS NULL OR
       p_recovered IS NULL OR p_recovered NOT IN (0, 1) OR
       p_previously_started IS NULL OR p_previously_started NOT IN (0, 1) THEN
        RAISE EXCEPTION 'waddle: malformed send attempt release'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    IF p_recovered = 1 AND p_previously_started = 1 THEN
        UPDATE __SCHEMA__.ingress_send_attempts AS a
        SET state = 1, expires_at_ms = 0
        WHERE a.message_key = p_message_key AND a.kind = p_kind
          AND a.semantic_identity_hash = p_semantic_identity_hash
          AND a.recipient = p_resource AND a.node_id = p_node_id
          AND a.node_incarnation = p_node_incarnation
          AND a.lease_token = p_lease_token AND a.state IN (0, 1);
    ELSIF p_recovered = 1 THEN
        UPDATE __SCHEMA__.ingress_send_attempts AS a
        SET state = 0, expires_at_ms = 0
        WHERE a.message_key = p_message_key AND a.kind = p_kind
          AND a.semantic_identity_hash = p_semantic_identity_hash
          AND a.recipient = p_resource AND a.node_id = p_node_id
          AND a.node_incarnation = p_node_incarnation
          AND a.lease_token = p_lease_token AND a.state IN (0, 1);
    ELSE
        DELETE FROM __SCHEMA__.ingress_send_attempts AS a
        WHERE a.message_key = p_message_key AND a.kind = p_kind
          AND a.semantic_identity_hash = p_semantic_identity_hash
          AND a.recipient = p_resource AND a.node_id = p_node_id
          AND a.node_incarnation = p_node_incarnation
          AND a.lease_token = p_lease_token AND a.state IN (0, 1);
    END IF;
    GET DIAGNOSTICS changed_rows = ROW_COUNT;
    RETURN changed_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_release_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID, INTEGER, INTEGER
) OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_release_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID, INTEGER, INTEGER
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_release_send_attempt(
    UUID, INTEGER, BYTEA, TEXT, TEXT, TEXT, UUID, INTEGER, INTEGER
) TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_retire_expired_send_attempt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    deleted_rows INTEGER;
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed send attempt retirement'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    DELETE FROM __SCHEMA__.ingress_send_attempts AS a
    WHERE a.message_key = p_message_key AND a.kind = p_kind
      AND a.semantic_identity_hash = p_semantic_identity_hash
      AND a.recipient = p_resource AND a.state IN (0, 1)
      AND a.expires_at_ms <= (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint;
    GET DIAGNOSTICS deleted_rows = ROW_COUNT;
    RETURN deleted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_retire_expired_send_attempt(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_retire_expired_send_attempt(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_retire_expired_send_attempt(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_delivery_receipt(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_policy_discard_reason TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows INTEGER;
BEGIN
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_delivery_receipts
        (message_key, kind, semantic_identity_hash, resource, policy_discard_reason)
    VALUES (p_message_key, p_kind, p_semantic_identity_hash, p_resource,
            p_policy_discard_reason)
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_delivery_receipt(UUID, INTEGER, BYTEA, TEXT, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_delivery_receipt(UUID, INTEGER, BYTEA, TEXT, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_delivery_receipt(UUID, INTEGER, BYTEA, TEXT, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_delivery_receipts(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_after_resource TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (resource TEXT, policy_discard_reason TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 THEN
        RAISE EXCEPTION 'waddle: malformed delivery receipt page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT d.resource, d.policy_discard_reason
    FROM __SCHEMA__.ingress_delivery_receipts AS d
    WHERE d.message_key = p_message_key AND d.kind = p_kind
      AND d.semantic_identity_hash = p_semantic_identity_hash
      AND (p_after_resource IS NULL OR d.resource > p_after_resource)
    ORDER BY d.resource
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_delivery_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_delivery_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_delivery_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_all_delivery_receipts(
    p_message_key UUID,
    p_after_kind INTEGER,
    p_after_hash BYTEA,
    p_after_resource TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (kind INTEGER, semantic_identity_hash BYTEA, resource TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_kind IS NULL) <> (p_after_hash IS NULL)) OR
       ((p_after_kind IS NULL) <> (p_after_resource IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed delivery receipt page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT d.kind, d.semantic_identity_hash, d.resource
    FROM __SCHEMA__.ingress_delivery_receipts AS d
    WHERE d.message_key = p_message_key
      AND (p_after_kind IS NULL OR
           (d.kind, d.semantic_identity_hash, d.resource) >
           (p_after_kind, p_after_hash, p_after_resource))
    ORDER BY d.kind, d.semantic_identity_hash, d.resource
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_all_delivery_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_all_delivery_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_all_delivery_receipts(UUID, INTEGER, BYTEA, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

ALTER FUNCTION __SCHEMA__.waddle_ingress_resolve_origin(
    UUID, INTEGER, BYTEA, BYTEA, TEXT, INTEGER, TEXT, TEXT,
    TEXT, UUID, NUMERIC, NUMERIC, UUID, NUMERIC
) OWNER TO __API_OWNER_IDENT__;

REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard() FROM PUBLIC;

REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_resolve_origin(
    UUID, INTEGER, BYTEA, BYTEA, TEXT, INTEGER, TEXT, TEXT,
    TEXT, UUID, NUMERIC, NUMERIC, UUID, NUMERIC
) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_resolve_origin(
    UUID, INTEGER, BYTEA, BYTEA, TEXT, INTEGER, TEXT, TEXT,
    TEXT, UUID, NUMERIC, NUMERIC, UUID, NUMERIC
) TO __RUNTIME_ROLE_IDENT__;

DROP TRIGGER IF EXISTS ingress_messages_definer_api_guard_dml ON __SCHEMA__.ingress_messages;
CREATE TRIGGER ingress_messages_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_messages
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_messages ENABLE ALWAYS TRIGGER ingress_messages_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_origin_aliases_definer_api_guard_dml ON __SCHEMA__.ingress_origin_aliases;
CREATE TRIGGER ingress_origin_aliases_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_origin_aliases
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_origin_aliases ENABLE ALWAYS TRIGGER ingress_origin_aliases_definer_api_guard_dml;

-- Archive dispatch is the durable ordering index for MAM-backed copies.  Its
-- fixed APIs keep registration, validation and readiness reads inside the
-- caller-owned ingress transaction.  Lists use keyset pages so an archive
-- with high fanout cannot force an unbounded result into Rust.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_insert_archive_dispatch(
    p_archive_jid TEXT,
    p_archive_seq BIGINT,
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_pending_row_id TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows BIGINT;
BEGIN
    IF p_archive_jid IS NULL OR p_archive_jid = '' OR p_archive_seq IS NULL OR
       p_archive_seq < 1 OR p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL OR
       p_pending_row_id IS NULL OR (p_pending_row_id <> '' AND p_resource <> '') THEN
        RAISE EXCEPTION 'waddle: malformed archive dispatch insert'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    INSERT INTO __SCHEMA__.ingress_archive_dispatch
        (archive_jid, archive_seq, message_key, kind, semantic_identity_hash, resource, pending_row_id)
    VALUES
        (p_archive_jid, p_archive_seq, p_message_key, p_kind,
         p_semantic_identity_hash, p_resource, p_pending_row_id)
    ON CONFLICT DO NOTHING;
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_insert_archive_dispatch(TEXT, BIGINT, UUID, INTEGER, BYTEA, TEXT, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_insert_archive_dispatch(TEXT, BIGINT, UUID, INTEGER, BYTEA, TEXT, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_insert_archive_dispatch(TEXT, BIGINT, UUID, INTEGER, BYTEA, TEXT, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_archive_dispatch_position(
    p_archive_jid TEXT,
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_pending_row_id TEXT
)
RETURNS BIGINT
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    stored_position BIGINT;
BEGIN
    IF p_archive_jid IS NULL OR p_archive_jid = '' OR p_message_key IS NULL OR
       p_kind IS NULL OR p_semantic_identity_hash IS NULL OR p_resource IS NULL OR
       p_pending_row_id IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed archive dispatch lookup'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    SELECT d.archive_seq INTO stored_position
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    WHERE d.archive_jid = p_archive_jid AND d.message_key = p_message_key
      AND d.kind = p_kind AND d.semantic_identity_hash = p_semantic_identity_hash
      AND d.resource = p_resource AND d.pending_row_id = p_pending_row_id;
    RETURN stored_position;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_archive_dispatch_position(TEXT, UUID, INTEGER, BYTEA, TEXT, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_archive_dispatch_position(TEXT, UUID, INTEGER, BYTEA, TEXT, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_archive_dispatch_position(TEXT, UUID, INTEGER, BYTEA, TEXT, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_archive_registration(
    p_archive_jid TEXT,
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_after_resource TEXT,
    p_after_pending_row_id TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (archive_seq BIGINT, resource TEXT, pending_row_id TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_archive_jid IS NULL OR p_archive_jid = '' OR p_message_key IS NULL OR
       p_kind IS NULL OR p_semantic_identity_hash IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_resource IS NULL) <> (p_after_pending_row_id IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed archive registration page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT d.archive_seq, d.resource, d.pending_row_id
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    WHERE d.archive_jid = p_archive_jid AND d.message_key = p_message_key
      AND d.kind = p_kind AND d.semantic_identity_hash = p_semantic_identity_hash
      AND (p_after_resource IS NULL OR
           (d.resource, d.pending_row_id) > (p_after_resource, p_after_pending_row_id))
    ORDER BY d.resource, d.pending_row_id
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_archive_registration(TEXT, UUID, INTEGER, BYTEA, TEXT, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_archive_registration(TEXT, UUID, INTEGER, BYTEA, TEXT, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_archive_registration(TEXT, UUID, INTEGER, BYTEA, TEXT, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_archive_positions(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_after_archive_jid TEXT,
    p_after_archive_seq BIGINT,
    p_page_size INTEGER
)
RETURNS TABLE (archive_jid TEXT, archive_seq BIGINT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR p_semantic_identity_hash IS NULL OR
       p_page_size IS NULL OR p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_archive_jid IS NULL) <> (p_after_archive_seq IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed archive position page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT DISTINCT d.archive_jid, d.archive_seq
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    WHERE d.message_key = p_message_key AND d.kind = p_kind
      AND d.semantic_identity_hash = p_semantic_identity_hash
      AND (p_after_archive_jid IS NULL OR
           (d.archive_jid, d.archive_seq) > (p_after_archive_jid, p_after_archive_seq))
    ORDER BY d.archive_jid, d.archive_seq
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_archive_positions(UUID, INTEGER, BYTEA, TEXT, BIGINT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_archive_positions(UUID, INTEGER, BYTEA, TEXT, BIGINT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_archive_positions(UUID, INTEGER, BYTEA, TEXT, BIGINT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_positions(
    p_archive_jid TEXT,
    p_pending_row_id TEXT,
    p_after_archive_seq BIGINT,
    p_after_resource TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (archive_jid TEXT, archive_seq BIGINT, resource TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_archive_jid IS NULL OR p_archive_jid = '' OR p_pending_row_id IS NULL OR
       p_pending_row_id = '' OR p_page_size IS NULL OR p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_archive_seq IS NULL) <> (p_after_resource IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed pending archive position page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT DISTINCT d.archive_jid, d.archive_seq, d.resource
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    WHERE d.archive_jid = p_archive_jid AND d.pending_row_id = p_pending_row_id
      AND (p_after_archive_seq IS NULL OR (d.archive_seq, d.resource) >
           (p_after_archive_seq, p_after_resource))
    ORDER BY d.archive_seq, d.resource
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_positions(TEXT, TEXT, BIGINT, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_positions(TEXT, TEXT, BIGINT, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_positions(TEXT, TEXT, BIGINT, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_receipt_archive_positions(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT,
    p_after_archive_jid TEXT,
    p_after_archive_seq BIGINT,
    p_after_resource TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (archive_jid TEXT, archive_seq BIGINT, resource TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR p_semantic_identity_hash IS NULL OR
       p_resource IS NULL OR p_page_size IS NULL OR p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_archive_jid IS NULL) <> (p_after_archive_seq IS NULL)) OR
       ((p_after_archive_jid IS NULL) <> (p_after_resource IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed receipt archive position page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT DISTINCT d.archive_jid, d.archive_seq, d.resource
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    WHERE d.message_key = p_message_key AND d.kind = p_kind
      AND d.semantic_identity_hash = p_semantic_identity_hash
      AND (p_resource = '' OR d.resource = '' OR d.resource = p_resource)
      AND (p_after_archive_jid IS NULL OR
           (d.archive_jid, d.archive_seq, d.resource) >
           (p_after_archive_jid, p_after_archive_seq, p_after_resource))
    ORDER BY d.archive_jid, d.archive_seq, d.resource
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_receipt_archive_positions(UUID, INTEGER, BYTEA, TEXT, TEXT, BIGINT, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_receipt_archive_positions(UUID, INTEGER, BYTEA, TEXT, TEXT, BIGINT, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_receipt_archive_positions(UUID, INTEGER, BYTEA, TEXT, TEXT, BIGINT, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_resource_receipt_exists(
    p_message_key UUID,
    p_kind INTEGER,
    p_semantic_identity_hash BYTEA,
    p_resource TEXT
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL OR p_kind IS NULL OR
       p_semantic_identity_hash IS NULL OR p_resource IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed resource receipt lookup'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_delivery_receipts AS d
        WHERE d.message_key = p_message_key AND d.kind = p_kind
          AND d.semantic_identity_hash = p_semantic_identity_hash AND d.resource = p_resource
        UNION ALL
        SELECT 1 FROM __SCHEMA__.ingress_carbon_receipts AS c
        WHERE c.message_key = p_message_key AND c.kind = p_kind
          AND c.semantic_identity_hash = p_semantic_identity_hash AND c.recipient = p_resource
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_resource_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_resource_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_resource_receipt_exists(UUID, INTEGER, BYTEA, TEXT)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_archive_predecessors(
    p_archive_jid TEXT,
    p_before_archive_seq BIGINT,
    p_resource TEXT,
    p_after_archive_seq BIGINT,
    p_after_message_key UUID,
    p_after_kind INTEGER,
    p_after_hash BYTEA,
    p_after_resource TEXT,
    p_after_pending_row_id TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (message_key TEXT, pending_row_id TEXT, receipted BOOLEAN, archive_seq BIGINT,
               kind INTEGER, semantic_identity_hash BYTEA, resource TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_archive_jid IS NULL OR p_archive_jid = '' OR p_before_archive_seq IS NULL OR
       p_before_archive_seq < 1 OR p_resource IS NULL OR p_page_size IS NULL OR
       p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_archive_seq IS NULL) <> (p_after_message_key IS NULL)) OR
       ((p_after_archive_seq IS NULL) <> (p_after_kind IS NULL)) OR
       ((p_after_archive_seq IS NULL) <> (p_after_hash IS NULL)) OR
       ((p_after_archive_seq IS NULL) <> (p_after_resource IS NULL)) OR
       ((p_after_archive_seq IS NULL) <> (p_after_pending_row_id IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed archive predecessor page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT d.message_key::TEXT, d.pending_row_id,
        EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS e
                WHERE e.message_key = d.message_key AND e.kind = d.kind
                  AND e.semantic_identity_hash = d.semantic_identity_hash),
        d.archive_seq, d.kind, d.semantic_identity_hash, d.resource
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    WHERE d.archive_jid = p_archive_jid AND d.archive_seq < p_before_archive_seq
      AND (p_resource = '' OR d.resource = '' OR d.resource = p_resource)
      AND (d.pending_row_id <> '' OR NOT EXISTS (
          SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS e
          WHERE e.message_key = d.message_key AND e.kind = d.kind
            AND e.semantic_identity_hash = d.semantic_identity_hash))
      AND NOT EXISTS (
          SELECT 1 FROM __SCHEMA__.ingress_delivery_receipts AS r
          WHERE d.resource <> '' AND r.message_key = d.message_key AND r.kind = d.kind
            AND r.semantic_identity_hash = d.semantic_identity_hash AND r.resource = d.resource)
      AND NOT EXISTS (
          SELECT 1 FROM __SCHEMA__.ingress_carbon_receipts AS r
          WHERE d.resource <> '' AND r.message_key = d.message_key AND r.kind = d.kind
            AND r.semantic_identity_hash = d.semantic_identity_hash AND r.recipient = d.resource)
      AND (p_after_archive_seq IS NULL OR
           (d.archive_seq, d.message_key, d.kind, d.semantic_identity_hash,
            d.resource, d.pending_row_id) >
           (p_after_archive_seq, p_after_message_key, p_after_kind, p_after_hash,
            p_after_resource, p_after_pending_row_id))
    ORDER BY d.archive_seq, d.message_key, d.kind, d.semantic_identity_hash,
             d.resource, d.pending_row_id
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_archive_predecessors(TEXT, BIGINT, TEXT, BIGINT, UUID, INTEGER, BYTEA, TEXT, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_archive_predecessors(TEXT, BIGINT, TEXT, BIGINT, UUID, INTEGER, BYTEA, TEXT, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_archive_predecessors(TEXT, BIGINT, TEXT, BIGINT, UUID, INTEGER, BYTEA, TEXT, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_sources(
    p_archive_jid TEXT,
    p_pending_row_id TEXT,
    p_after_message_key UUID,
    p_after_kind INTEGER,
    p_after_hash BYTEA,
    p_after_pending_row_id TEXT,
    p_page_size INTEGER
)
RETURNS TABLE (message_key TEXT, kind INTEGER, semantic_identity_hash BYTEA,
               payload_version INTEGER, payload BYTEA, pending_row_id TEXT)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_archive_jid IS NULL OR p_archive_jid = '' OR p_pending_row_id IS NULL OR
       p_pending_row_id = '' OR p_page_size IS NULL OR p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_message_key IS NULL) <> (p_after_kind IS NULL)) OR
       ((p_after_message_key IS NULL) <> (p_after_hash IS NULL)) OR
       ((p_after_message_key IS NULL) <> (p_after_pending_row_id IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed pending archive source page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT d.message_key::TEXT, d.kind, d.semantic_identity_hash,
           i.payload_version::INTEGER, i.payload, d.pending_row_id
    FROM __SCHEMA__.ingress_archive_dispatch AS d
    JOIN __SCHEMA__.ingress_effect_intents AS i
      ON i.message_key = d.message_key AND i.kind = d.kind
     AND i.semantic_identity_hash = d.semantic_identity_hash
    WHERE d.archive_jid = p_archive_jid AND d.pending_row_id <> ''
      AND d.archive_seq IN (
          SELECT pending.archive_seq
          FROM __SCHEMA__.ingress_archive_dispatch AS pending
          WHERE pending.archive_jid = p_archive_jid
            AND pending.pending_row_id = p_pending_row_id)
      AND (p_after_message_key IS NULL OR
           (d.message_key, d.kind, d.semantic_identity_hash, d.pending_row_id) >
           (p_after_message_key, p_after_kind, p_after_hash, p_after_pending_row_id))
    ORDER BY d.message_key, d.kind, d.semantic_identity_hash, d.pending_row_id
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_sources(TEXT, TEXT, UUID, INTEGER, BYTEA, TEXT, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_sources(TEXT, TEXT, UUID, INTEGER, BYTEA, TEXT, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_archive_sources(TEXT, TEXT, UUID, INTEGER, BYTEA, TEXT, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_list_pending_route_sources(
    p_route_kind INTEGER,
    p_archive_kind INTEGER,
    p_archive_hash BYTEA,
    p_after_message_key UUID,
    p_after_route_hash BYTEA,
    p_page_size INTEGER
)
RETURNS TABLE (message_key TEXT, route_kind INTEGER, route_hash BYTEA,
               route_payload_version INTEGER, route_payload BYTEA,
               archive_payload_version INTEGER, archive_payload BYTEA)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_route_kind IS NULL OR p_archive_kind IS NULL OR p_archive_hash IS NULL OR
       p_page_size IS NULL OR p_page_size < 1 OR p_page_size > 256 OR
       ((p_after_message_key IS NULL) <> (p_after_route_hash IS NULL)) THEN
        RAISE EXCEPTION 'waddle: malformed pending route source page request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN QUERY
    SELECT route.message_key::TEXT, route.kind, route.semantic_identity_hash,
           route.payload_version::INTEGER, route.payload,
           archive.payload_version::INTEGER, archive.payload
    FROM __SCHEMA__.ingress_effect_intents AS route
    JOIN __SCHEMA__.ingress_effect_intents AS archive
      ON archive.message_key = route.message_key
    WHERE route.kind = p_route_kind AND archive.kind = p_archive_kind
      AND archive.semantic_identity_hash = p_archive_hash
      AND (p_after_message_key IS NULL OR
           (route.message_key, route.semantic_identity_hash) >
           (p_after_message_key, p_after_route_hash))
    ORDER BY route.message_key, route.semantic_identity_hash
    LIMIT p_page_size;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_list_pending_route_sources(INTEGER, INTEGER, BYTEA, UUID, BYTEA, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_route_sources(INTEGER, INTEGER, BYTEA, UUID, BYTEA, INTEGER)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_list_pending_route_sources(INTEGER, INTEGER, BYTEA, UUID, BYTEA, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_owner_rejection_child(p_source_message_key UUID)
RETURNS TEXT
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    child_key TEXT;
BEGIN
    IF p_source_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed owner rejection lookup'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    SELECT r.child_message_key::TEXT INTO child_key
    FROM __SCHEMA__.ingress_owner_rejections AS r
    WHERE r.source_message_key = p_source_message_key;
    RETURN child_key;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_owner_rejection_child(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_owner_rejection_child(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_owner_rejection_child(UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_record_owner_rejection(
    p_source_message_key UUID,
    p_child_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    inserted_rows BIGINT;
BEGIN
    IF p_source_message_key IS NULL OR p_child_message_key IS NULL OR
       p_source_message_key = p_child_message_key THEN
        RAISE EXCEPTION 'waddle: malformed owner rejection insert'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    IF NOT EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = p_source_message_key
          AND m.source_binding_version = 1 AND m.owner_decision = 0
    ) THEN
        RETURN FALSE;
    END IF;
    INSERT INTO __SCHEMA__.ingress_owner_rejections
        (source_message_key, child_message_key)
    VALUES (p_source_message_key, p_child_message_key);
    GET DIAGNOSTICS inserted_rows = ROW_COUNT;
    RETURN inserted_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_record_owner_rejection(UUID, UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_record_owner_rejection(UUID, UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_record_owner_rejection(UUID, UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_initialize_owner_decision(
    p_source_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows BIGINT;
BEGIN
    IF p_source_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed owner decision initialization'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_messages
    SET owner_decision = 0
    WHERE message_key = p_source_message_key AND owner_decision IS NULL
      AND source_binding_version = 1;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_initialize_owner_decision(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_initialize_owner_decision(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_initialize_owner_decision(UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_decide_owner(
    p_source_message_key UUID,
    p_decision INTEGER
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows BIGINT;
BEGIN
    IF p_source_message_key IS NULL OR p_decision IS NULL OR p_decision NOT IN (1, 2) THEN
        RAISE EXCEPTION 'waddle: malformed owner decision'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_messages
    SET owner_decision = p_decision
    WHERE message_key = p_source_message_key AND owner_decision = 0;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_decide_owner(UUID, INTEGER)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_decide_owner(UUID, INTEGER) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_decide_owner(UUID, INTEGER)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_owner_result_retains_source(
    p_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed owner retention lookup'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    RETURN EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = p_message_key
          AND (EXISTS (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections AS child
                       WHERE child.source_message_key = m.message_key)
               OR m.owner_decision = 2
               OR (m.source_binding_version IS NOT NULL
                   AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                   AND EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS i
                               WHERE i.message_key = m.message_key AND i.kind = 12)))
    );
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_owner_result_retains_source(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_owner_result_retains_source(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_owner_result_retains_source(UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_invalidate_retention(
    p_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows BIGINT;
BEGIN
    IF p_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed retention invalidation'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    UPDATE __SCHEMA__.ingress_messages AS m
    SET retention_eligible_at = NULL
    WHERE m.message_key = p_message_key;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_invalidate_retention(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_invalidate_retention(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_invalidate_retention(UUID)
    TO __RUNTIME_ROLE_IDENT__;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(
    p_message_key UUID
)
RETURNS BOOLEAN
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    updated_rows BIGINT;
    db_now TIMESTAMPTZ;
BEGIN
    IF p_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed retention refresh'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    db_now := pg_catalog.clock_timestamp();
    IF pg_catalog.to_regclass('__SCHEMA_PATH__.pending_delivery') IS NOT NULL THEN
        UPDATE __SCHEMA__.ingress_messages AS m
        SET retention_eligible_at = CASE WHEN m.terminal_at IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_sm_refs AS r
                          WHERE r.message_key = m.message_key)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_mailbox_entries AS entry
                          WHERE entry.message_key = m.message_key AND entry.disposition < 3)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections AS owner_child
                          WHERE owner_child.source_message_key = m.message_key
                            AND (m.owner_decision IS NULL OR m.owner_decision <> 2))
          AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR EXISTS
               (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections AS owner_child
                WHERE owner_child.source_message_key = m.message_key))
          AND NOT (m.source_binding_version IS NOT NULL
                   AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                   AND EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS owner_intent
                               WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12))
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_archive_dispatch AS dispatch
                          JOIN __SCHEMA__.pending_delivery AS pending
                            ON pending.row_id = dispatch.pending_row_id
                          WHERE dispatch.message_key = m.message_key
                            AND dispatch.pending_row_id <> '')
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_descendants AS d
                          WHERE d.message_key = m.message_key AND d.settled_at IS NULL)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS i
                          WHERE i.message_key = m.message_key AND NOT EXISTS
                            (SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
                             WHERE r.message_key = i.message_key AND r.kind = i.kind
                               AND r.semantic_identity_hash = i.semantic_identity_hash))
          THEN COALESCE(m.retention_eligible_at, db_now) ELSE NULL END
        WHERE m.message_key = p_message_key;
    ELSE
        UPDATE __SCHEMA__.ingress_messages AS m
        SET retention_eligible_at = CASE WHEN m.terminal_at IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_sm_refs AS r
                          WHERE r.message_key = m.message_key)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_mailbox_entries AS entry
                          WHERE entry.message_key = m.message_key AND entry.disposition < 3)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections AS owner_child
                          WHERE owner_child.source_message_key = m.message_key
                            AND (m.owner_decision IS NULL OR m.owner_decision <> 2))
          AND (m.owner_decision IS NULL OR m.owner_decision <> 2 OR EXISTS
               (SELECT 1 FROM __SCHEMA__.ingress_owner_rejections AS owner_child
                WHERE owner_child.source_message_key = m.message_key))
          AND NOT (m.source_binding_version IS NOT NULL
                   AND (m.owner_decision IS NULL OR m.owner_decision = 0)
                   AND EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS owner_intent
                               WHERE owner_intent.message_key = m.message_key AND owner_intent.kind = 12))
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_descendants AS d
                          WHERE d.message_key = m.message_key AND d.settled_at IS NULL)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS i
                          WHERE i.message_key = m.message_key AND NOT EXISTS
                            (SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
                             WHERE r.message_key = i.message_key AND r.kind = i.kind
                               AND r.semantic_identity_hash = i.semantic_identity_hash))
          THEN COALESCE(m.retention_eligible_at, db_now) ELSE NULL END
        WHERE m.message_key = p_message_key;
    END IF;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(UUID)
    TO __RUNTIME_ROLE_IDENT__;

-- Standalone post-commit completion is one closed operation. The canonical
-- parent lock serializes cooperating intent/child mutators and GC; each query
-- after a lock wait takes a fresh READ COMMITTED snapshot.
-- Results: 0 incomplete, 1 newly terminal, 2 already terminal, 3 absent.
CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_terminalize_if_complete(
    p_message_key UUID
)
RETURNS INTEGER
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    was_terminal BOOLEAN;
BEGIN
    IF p_message_key IS NULL THEN
        RAISE EXCEPTION 'waddle: malformed terminalization request'
            USING ERRCODE = '22023';
    END IF;
    PERFORM __SCHEMA__.waddle_ingress_definer_prepare();
    SELECT m.terminal_at IS NOT NULL INTO was_terminal
    FROM __SCHEMA__.ingress_messages AS m
    WHERE m.message_key = p_message_key
    FOR UPDATE OF m;
    IF NOT FOUND THEN
        RETURN 3;
    END IF;
    IF EXISTS (
        SELECT 1 FROM __SCHEMA__.ingress_effect_intents AS i
        WHERE i.message_key = p_message_key
          AND NOT EXISTS (
              SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
              WHERE r.message_key = i.message_key
                AND r.kind = i.kind
                AND r.semantic_identity_hash = i.semantic_identity_hash)
    ) THEN
        RETURN 0;
    END IF;
    IF was_terminal THEN
        RETURN 2;
    END IF;
    UPDATE __SCHEMA__.ingress_messages
    SET terminal_at = pg_catalog.clock_timestamp()
    WHERE message_key = p_message_key;
    PERFORM __SCHEMA__.waddle_ingress_refresh_retention(p_message_key);
    RETURN 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_terminalize_if_complete(UUID)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_terminalize_if_complete(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_terminalize_if_complete(UUID)
    TO __RUNTIME_ROLE_IDENT__;

DROP FUNCTION IF EXISTS __SCHEMA__.waddle_ingress_settle_descendant(UUID, TIMESTAMPTZ);
DROP FUNCTION IF EXISTS __SCHEMA__.waddle_ingress_refresh_retention(UUID, TIMESTAMPTZ);

DO $pending_delivery_api_grant$
BEGIN
    IF pg_catalog.to_regclass('__SCHEMA_PATH__.pending_delivery') IS NOT NULL THEN
        EXECUTE 'GRANT SELECT ON TABLE __SCHEMA__.pending_delivery TO __API_OWNER_IDENT__';
    END IF;
END
$pending_delivery_api_grant$;

-- The additional candidate trigger keeps the existing V1009 epoch guard and
-- immutable triggers intact while requiring every guarded DML statement to
-- originate inside a fixed API-owner function.
DROP TRIGGER IF EXISTS ingress_sm_refs_definer_api_guard_dml ON __SCHEMA__.ingress_sm_refs;
CREATE TRIGGER ingress_sm_refs_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_sm_refs
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_sm_refs ENABLE ALWAYS TRIGGER ingress_sm_refs_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_deliveries_definer_api_guard_dml ON __SCHEMA__.ingress_deliveries;
CREATE TRIGGER ingress_deliveries_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_deliveries
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_deliveries ENABLE ALWAYS TRIGGER ingress_deliveries_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_sm_streams_definer_api_guard_dml ON __SCHEMA__.ingress_sm_streams;
CREATE TRIGGER ingress_sm_streams_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_sm_streams
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_sm_streams ENABLE ALWAYS TRIGGER ingress_sm_streams_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_effect_intents_definer_api_guard_dml ON __SCHEMA__.ingress_effect_intents;
CREATE TRIGGER ingress_effect_intents_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_effect_intents
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_effect_intents ENABLE ALWAYS TRIGGER ingress_effect_intents_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_effect_receipts_definer_api_guard_dml ON __SCHEMA__.ingress_effect_receipts;
CREATE TRIGGER ingress_effect_receipts_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_effect_receipts
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_effect_receipts ENABLE ALWAYS TRIGGER ingress_effect_receipts_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_carbon_receipts_definer_api_guard_dml ON __SCHEMA__.ingress_carbon_receipts;
CREATE TRIGGER ingress_carbon_receipts_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_carbon_receipts
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_carbon_receipts ENABLE ALWAYS TRIGGER ingress_carbon_receipts_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_delivery_receipts_definer_api_guard_dml ON __SCHEMA__.ingress_delivery_receipts;
CREATE TRIGGER ingress_delivery_receipts_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_delivery_receipts
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_delivery_receipts ENABLE ALWAYS TRIGGER ingress_delivery_receipts_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_archive_dispatch_definer_api_guard_dml ON __SCHEMA__.ingress_archive_dispatch;
CREATE TRIGGER ingress_archive_dispatch_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_archive_dispatch
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_archive_dispatch ENABLE ALWAYS TRIGGER ingress_archive_dispatch_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_send_attempts_definer_api_guard_dml ON __SCHEMA__.ingress_send_attempts;
CREATE TRIGGER ingress_send_attempts_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_send_attempts
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_send_attempts ENABLE ALWAYS TRIGGER ingress_send_attempts_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_effect_descendants_definer_api_guard_dml ON __SCHEMA__.ingress_effect_descendants;
CREATE TRIGGER ingress_effect_descendants_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_effect_descendants
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_effect_descendants ENABLE ALWAYS TRIGGER ingress_effect_descendants_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_owner_rejections_definer_api_guard_dml ON __SCHEMA__.ingress_owner_rejections;
CREATE TRIGGER ingress_owner_rejections_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_owner_rejections
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_owner_rejections ENABLE ALWAYS TRIGGER ingress_owner_rejections_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_mailbox_lanes_definer_api_guard_dml ON __SCHEMA__.ingress_mailbox_lanes;
CREATE TRIGGER ingress_mailbox_lanes_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_mailbox_lanes
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_mailbox_lanes ENABLE ALWAYS TRIGGER ingress_mailbox_lanes_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_mailbox_entries_definer_api_guard_dml ON __SCHEMA__.ingress_mailbox_entries;
CREATE TRIGGER ingress_mailbox_entries_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_mailbox_entries
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_mailbox_entries ENABLE ALWAYS TRIGGER ingress_mailbox_entries_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_room_requests_definer_api_guard_dml ON __SCHEMA__.ingress_room_requests;
CREATE TRIGGER ingress_room_requests_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_room_requests
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_room_requests ENABLE ALWAYS TRIGGER ingress_room_requests_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_room_responses_definer_api_guard_dml ON __SCHEMA__.ingress_room_responses;
CREATE TRIGGER ingress_room_responses_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_room_responses
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_room_responses ENABLE ALWAYS TRIGGER ingress_room_responses_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_room_occupants_definer_api_guard_dml ON __SCHEMA__.ingress_room_occupants;
CREATE TRIGGER ingress_room_occupants_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_room_occupants
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_room_occupants ENABLE ALWAYS TRIGGER ingress_room_occupants_definer_api_guard_dml;

DROP TRIGGER IF EXISTS ingress_direct_requests_definer_api_guard_dml ON __SCHEMA__.ingress_direct_requests;
CREATE TRIGGER ingress_direct_requests_definer_api_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON __SCHEMA__.ingress_direct_requests
FOR EACH STATEMENT EXECUTE FUNCTION __SCHEMA__.waddle_ingress_definer_epoch_guard();
ALTER TABLE __SCHEMA__.ingress_direct_requests ENABLE ALWAYS TRIGGER ingress_direct_requests_definer_api_guard_dml;
