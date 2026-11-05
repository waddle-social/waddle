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
