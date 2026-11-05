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
            RETURN QUERY SELECT NULL::UUID, 'message_key_conflict'::TEXT, NULL::INTEGER, NULL::BYTEA;
            RETURN;
        END IF;

        INSERT INTO __SCHEMA__.ingress_origin_aliases (
            alias_key_hash, sender_bare_jid, target_kind, target_jid, origin_id, message_key
        ) VALUES (
            p_alias_key_hash, p_sender_bare_jid, p_target_kind, p_target_jid,
            p_origin_id, p_message_key
        )
        ON CONFLICT (alias_key_hash) DO NOTHING;
        GET DIAGNOSTICS alias_rows = ROW_COUNT;
        IF alias_rows = 1 THEN
            RETURN QUERY SELECT p_message_key, 'inserted'::TEXT, p_digest_version, p_digest;
            RETURN;
        END IF;

        -- A concurrent first writer won the alias. Remove this transaction's
        -- unreferenced candidate before observing the winner under a fresh
        -- READ COMMITTED statement snapshot.
        DELETE FROM __SCHEMA__.ingress_messages AS m
        WHERE m.message_key = p_message_key;
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

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(
    p_message_key UUID,
    p_now TIMESTAMPTZ
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
    IF pg_catalog.to_regclass('__SCHEMA_PATH__.pending_delivery') IS NOT NULL THEN
        UPDATE __SCHEMA__.ingress_messages AS m
        SET retention_eligible_at = CASE WHEN m.terminal_at IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_sm_refs AS r WHERE r.message_key = m.message_key)
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.pending_delivery AS pending
              JOIN __SCHEMA__.ingress_archive_dispatch AS dispatch
                ON dispatch.pending_row_id = pending.row_id
              WHERE dispatch.message_key = m.message_key AND dispatch.pending_row_id <> '')
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
              WHERE i.message_key = m.message_key AND NOT EXISTS (
                  SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
                  WHERE r.message_key = i.message_key AND r.kind = i.kind
                    AND r.semantic_identity_hash = i.semantic_identity_hash))
          THEN COALESCE(m.retention_eligible_at, p_now) ELSE NULL END
        WHERE m.message_key = p_message_key;
    ELSE
        UPDATE __SCHEMA__.ingress_messages AS m
        SET retention_eligible_at = CASE WHEN m.terminal_at IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM __SCHEMA__.ingress_sm_refs AS r WHERE r.message_key = m.message_key)
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
              WHERE i.message_key = m.message_key AND NOT EXISTS (
                  SELECT 1 FROM __SCHEMA__.ingress_effect_receipts AS r
                  WHERE r.message_key = i.message_key AND r.kind = i.kind
                    AND r.semantic_identity_hash = i.semantic_identity_hash))
          THEN COALESCE(m.retention_eligible_at, p_now) ELSE NULL END
        WHERE m.message_key = p_message_key;
    END IF;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    RETURN updated_rows = 1;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(UUID, TIMESTAMPTZ)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_refresh_retention(UUID, TIMESTAMPTZ)
    FROM PUBLIC;

CREATE OR REPLACE FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(
    p_descendant_key UUID,
    p_now TIMESTAMPTZ
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

    UPDATE __SCHEMA__.ingress_effect_descendants AS d
    SET settled_at = p_now
    WHERE d.descendant_key = p_descendant_key::TEXT
      AND d.settled_at IS NULL;

    FOREACH ancestor_key IN ARRAY ancestor_keys LOOP
        PERFORM __SCHEMA__.waddle_ingress_refresh_retention(ancestor_key, p_now);
    END LOOP;
    RETURN TRUE;
END
$$;

ALTER FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(UUID, TIMESTAMPTZ)
    OWNER TO __API_OWNER_IDENT__;
REVOKE ALL ON FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(UUID, TIMESTAMPTZ)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION __SCHEMA__.waddle_ingress_settle_descendant(UUID, TIMESTAMPTZ)
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
