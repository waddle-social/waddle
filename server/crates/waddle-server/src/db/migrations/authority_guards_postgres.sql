-- The durable stream-management session authority and the account resource
-- binding store, under the ingress protocol epoch guard.
--
-- A write to any of the four relations needs the transaction-local epoch
-- proof at a nonzero epoch, so a binary that does not prove the live epoch
-- fails inside its own transaction instead of writing session or binding
-- state beside a binary that does.
--
-- The relations are created here so the guard exists from the first row.
-- Each store's owner initializer issues the same `CREATE ... IF NOT EXISTS`
-- statements for SQLite and for a PostgreSQL database without the migration
-- catalog; on a migrated database they find the relations present. A
-- database whose owner phase created the relations before this version
-- keeps them, and every statement below is repeatable for that reason.
--
-- No monitoring role is granted a read: the rows carry resume-token digests,
-- full JIDs and presence payloads, and no monitoring query reads them.
CREATE TABLE IF NOT EXISTS sm_durable_sessions (
    session_id TEXT PRIMARY KEY,
    resume_token_sha256 TEXT NOT NULL,
    full_jid TEXT NOT NULL,
    state INTEGER NOT NULL CHECK (state IN (1, 2, 3, 4)),
    terminal_reason INTEGER,
    generation BIGINT NOT NULL,
    binding TEXT NOT NULL,
    bare_jid TEXT NOT NULL,
    auth_context_id TEXT NOT NULL,
    auth_context_version BIGINT NOT NULL,
    principal_auth_epoch BIGINT NOT NULL,
    owner_node_id TEXT,
    owner_node_epoch TEXT,
    claim_epoch BIGINT,
    inbound_handled BIGINT NOT NULL,
    outbound_acked BIGINT NOT NULL,
    outbound_exposure_ceiling BIGINT NOT NULL,
    outbound_appended BIGINT NOT NULL,
    outbound_unacked_bytes BIGINT NOT NULL,
    max_resume_secs BIGINT NOT NULL,
    detached_at_ms BIGINT,
    enable_operation TEXT NOT NULL,
    last_operation TEXT NOT NULL,
    updated_at_ms BIGINT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sm_durable_sessions_token
    ON sm_durable_sessions (resume_token_sha256);
CREATE INDEX IF NOT EXISTS idx_sm_durable_sessions_detached
    ON sm_durable_sessions (state, detached_at_ms);
CREATE INDEX IF NOT EXISTS idx_sm_durable_sessions_resource
    ON sm_durable_sessions (full_jid);

CREATE TABLE IF NOT EXISTS sm_durable_operations (
    operation_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    kind INTEGER NOT NULL CHECK (kind IN (1, 2, 3)),
    outcome INTEGER NOT NULL CHECK (outcome IN (1, 2)),
    generation BIGINT NOT NULL,
    ordinal BIGINT,
    recorded_at_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sm_durable_operations_session
    ON sm_durable_operations (session_id, kind, ordinal);

CREATE TABLE IF NOT EXISTS account_resource_bindings (
    full_jid TEXT PRIMARY KEY,
    bare_jid TEXT NOT NULL,
    claim_identity TEXT NOT NULL,
    revision BIGINT NOT NULL,
    install_generation BIGINT NOT NULL,
    operation_seq BIGINT NOT NULL,
    state INTEGER NOT NULL CHECK (state IN (0, 1, 2, 3, 4, 5)),
    binding_ref TEXT,
    binding_claim_identity TEXT,
    logical_generation TEXT,
    occupancy_generation TEXT,
    endpoint_published INTEGER NOT NULL,
    actor_registered INTEGER NOT NULL,
    presence_available INTEGER NOT NULL,
    presence_show INTEGER,
    presence_status TEXT,
    presence_priority INTEGER NOT NULL,
    presence_payloads TEXT,
    carbons_enabled INTEGER NOT NULL,
    roster_interested INTEGER NOT NULL,
    blocklist_interested INTEGER NOT NULL,
    sm_session_id TEXT,
    sm_generation BIGINT,
    snapshot_revision BIGINT NOT NULL,
    detach_custody INTEGER,
    retiring_from INTEGER,
    retirement_source_revision BIGINT,
    retirement_custody INTEGER,
    offline_flushed INTEGER NOT NULL,
    subscriptions_flushed INTEGER NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    CONSTRAINT account_resource_bindings_sm_session_fk
        FOREIGN KEY (sm_session_id)
        REFERENCES sm_durable_sessions (session_id)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX IF NOT EXISTS idx_account_resource_bindings_account
    ON account_resource_bindings (bare_jid);
CREATE INDEX IF NOT EXISTS idx_account_resource_bindings_sm_session
    ON account_resource_bindings (sm_session_id);

CREATE TABLE IF NOT EXISTS account_resource_operations (
    operation_id TEXT PRIMARY KEY,
    full_jid TEXT NOT NULL,
    kind INTEGER NOT NULL,
    binding_ref TEXT NOT NULL,
    expected_revision BIGINT NOT NULL,
    committed_revision BIGINT NOT NULL,
    operation_seq BIGINT NOT NULL,
    recorded_at_ms BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_account_resource_operations_resource
    ON account_resource_operations (full_jid, operation_seq);

DROP TRIGGER IF EXISTS sm_durable_sessions_epoch_guard_dml ON sm_durable_sessions;
CREATE TRIGGER sm_durable_sessions_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON sm_durable_sessions
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
DROP TRIGGER IF EXISTS sm_durable_sessions_epoch_guard_truncate ON sm_durable_sessions;
CREATE TRIGGER sm_durable_sessions_epoch_guard_truncate
BEFORE TRUNCATE ON sm_durable_sessions
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE sm_durable_sessions ENABLE ALWAYS TRIGGER sm_durable_sessions_epoch_guard_dml;
ALTER TABLE sm_durable_sessions ENABLE ALWAYS TRIGGER sm_durable_sessions_epoch_guard_truncate;

DROP TRIGGER IF EXISTS sm_durable_operations_epoch_guard_dml ON sm_durable_operations;
CREATE TRIGGER sm_durable_operations_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON sm_durable_operations
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
DROP TRIGGER IF EXISTS sm_durable_operations_epoch_guard_truncate ON sm_durable_operations;
CREATE TRIGGER sm_durable_operations_epoch_guard_truncate
BEFORE TRUNCATE ON sm_durable_operations
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE sm_durable_operations ENABLE ALWAYS TRIGGER sm_durable_operations_epoch_guard_dml;
ALTER TABLE sm_durable_operations ENABLE ALWAYS TRIGGER sm_durable_operations_epoch_guard_truncate;

DROP TRIGGER IF EXISTS account_resource_bindings_epoch_guard_dml ON account_resource_bindings;
CREATE TRIGGER account_resource_bindings_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON account_resource_bindings
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
DROP TRIGGER IF EXISTS account_resource_bindings_epoch_guard_truncate ON account_resource_bindings;
CREATE TRIGGER account_resource_bindings_epoch_guard_truncate
BEFORE TRUNCATE ON account_resource_bindings
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE account_resource_bindings ENABLE ALWAYS TRIGGER account_resource_bindings_epoch_guard_dml;
ALTER TABLE account_resource_bindings ENABLE ALWAYS TRIGGER account_resource_bindings_epoch_guard_truncate;

DROP TRIGGER IF EXISTS account_resource_operations_epoch_guard_dml ON account_resource_operations;
CREATE TRIGGER account_resource_operations_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON account_resource_operations
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
DROP TRIGGER IF EXISTS account_resource_operations_epoch_guard_truncate ON account_resource_operations;
CREATE TRIGGER account_resource_operations_epoch_guard_truncate
BEFORE TRUNCATE ON account_resource_operations
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE account_resource_operations ENABLE ALWAYS TRIGGER account_resource_operations_epoch_guard_dml;
ALTER TABLE account_resource_operations ENABLE ALWAYS TRIGGER account_resource_operations_epoch_guard_truncate;

INSERT INTO ingress_epoch_guard_manifest (table_name)
VALUES
    ('sm_durable_sessions'),
    ('sm_durable_operations'),
    ('account_resource_bindings'),
    ('account_resource_operations')
ON CONFLICT (table_name) DO NOTHING;
