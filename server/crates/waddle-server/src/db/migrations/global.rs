use super::Migration;

/// Hard-cut schema reset for native OIDC/OAuth auth broker.
pub const V0001_AUTH_BROKER_SCHEMA: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS auth_identities;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS permission_tuples;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS native_users;
DROP TABLE IF EXISTS vcard_storage;
DROP TABLE IF EXISTS upload_slots;
DROP TABLE IF EXISTS link_preview_media_refs;
DROP TABLE IF EXISTS roster_items;
DROP TABLE IF EXISTS roster_versions;
DROP TABLE IF EXISTS blocking_list;
DROP TABLE IF EXISTS private_xml_storage;
DROP TABLE IF EXISTS provider_webhook_deliveries;
DROP TABLE IF EXISTS user_avatar_fetch_state;
DROP TABLE IF EXISTS user_avatar_source;

CREATE TABLE users (
    jid TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    xmpp_localpart TEXT NOT NULL UNIQUE,
    display_name TEXT,
    avatar_url TEXT,
    primary_email TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE auth_identities (
    id TEXT PRIMARY KEY,
    user_jid TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT,
    email_verified INTEGER,
    raw_claims_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_login_at TEXT NOT NULL,
    UNIQUE(issuer, subject),
    FOREIGN KEY (user_jid) REFERENCES users(jid) ON DELETE CASCADE
);

CREATE INDEX idx_auth_identities_user_jid ON auth_identities(user_jid);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    user_jid TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    FOREIGN KEY (user_jid) REFERENCES users(jid) ON DELETE CASCADE
);

CREATE INDEX idx_sessions_user_jid ON sessions(user_jid);
CREATE INDEX idx_sessions_expires_at ON sessions(expires_at);

CREATE TABLE permission_tuples (
    id TEXT PRIMARY KEY,
    object_type TEXT NOT NULL,
    object_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    subject_relation TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(object_type, object_id, relation, subject_type, subject_id, subject_relation)
);

CREATE INDEX idx_tuples_object ON permission_tuples(object_type, object_id);
CREATE INDEX idx_tuples_subject ON permission_tuples(subject_type, subject_id);
CREATE INDEX idx_tuples_relation ON permission_tuples(object_type, relation);
CREATE INDEX idx_tuples_check ON permission_tuples(object_type, object_id, relation, subject_type, subject_id);

CREATE TABLE native_users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,
    domain TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    salt TEXT NOT NULL,
    iterations INTEGER NOT NULL DEFAULT 4096,
    stored_key BLOB NOT NULL,
    server_key BLOB NOT NULL,
    email TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(username, domain)
);

CREATE INDEX idx_native_users_username_domain ON native_users(username, domain);
CREATE INDEX idx_native_users_email ON native_users(email) WHERE email IS NOT NULL;

CREATE TABLE vcard_storage (
    jid TEXT PRIMARY KEY,
    vcard_xml TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE upload_slots (
    id TEXT PRIMARY KEY,
    requester_jid TEXT NOT NULL,
    filename TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    content_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    storage_key TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL,
    uploaded_at TEXT
);

CREATE INDEX idx_upload_slots_requester ON upload_slots(requester_jid);
CREATE INDEX idx_upload_slots_expires ON upload_slots(expires_at) WHERE status = 'pending';
CREATE INDEX idx_upload_slots_status ON upload_slots(status);

CREATE TABLE roster_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_jid TEXT NOT NULL,
    contact_jid TEXT NOT NULL,
    name TEXT,
    subscription TEXT NOT NULL DEFAULT 'none',
    ask TEXT,
    approved BOOLEAN NOT NULL DEFAULT 0,
    groups TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(user_jid, contact_jid)
);

CREATE INDEX idx_roster_items_user ON roster_items(user_jid);
CREATE INDEX idx_roster_items_contact ON roster_items(contact_jid);
CREATE INDEX idx_roster_items_subscription ON roster_items(user_jid, subscription);

CREATE TABLE roster_versions (
    user_jid TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE blocking_list (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_jid TEXT NOT NULL,
    blocked_jid TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(user_jid, blocked_jid)
);

CREATE INDEX idx_blocking_list_user ON blocking_list(user_jid);
CREATE INDEX idx_blocking_list_blocked ON blocking_list(blocked_jid);

CREATE TABLE private_xml_storage (
    jid TEXT NOT NULL,
    namespace TEXT NOT NULL,
    xml_content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (jid, namespace)
);

PRAGMA foreign_keys = ON;
"#;

/// Hard-cut schema reset for native OIDC/OAuth auth broker — Postgres dialect.
///
/// Differences from the SQLite variant:
/// - No PRAGMA (not supported in Postgres)
/// - DROP TABLE ... CASCADE to handle FK-dependent drops
/// - BIGSERIAL instead of INTEGER PRIMARY KEY AUTOINCREMENT
/// - BYTEA instead of BLOB
/// - CURRENT_TIMESTAMP::TEXT for TEXT timestamp defaults
pub const V0001_AUTH_BROKER_SCHEMA_POSTGRES: &str = r#"
DROP TABLE IF EXISTS auth_identities CASCADE;
DROP TABLE IF EXISTS sessions CASCADE;
DROP TABLE IF EXISTS permission_tuples CASCADE;
DROP TABLE IF EXISTS users CASCADE;
DROP TABLE IF EXISTS native_users CASCADE;
DROP TABLE IF EXISTS vcard_storage CASCADE;
DROP TABLE IF EXISTS upload_slots CASCADE;
DROP TABLE IF EXISTS link_preview_media_refs CASCADE;
DROP TABLE IF EXISTS roster_items CASCADE;
DROP TABLE IF EXISTS roster_versions CASCADE;
DROP TABLE IF EXISTS blocking_list CASCADE;
DROP TABLE IF EXISTS private_xml_storage CASCADE;
DROP TABLE IF EXISTS provider_webhook_deliveries CASCADE;
DROP TABLE IF EXISTS user_avatar_fetch_state CASCADE;
DROP TABLE IF EXISTS user_avatar_source CASCADE;

CREATE TABLE users (
    jid TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    xmpp_localpart TEXT NOT NULL UNIQUE,
    display_name TEXT,
    avatar_url TEXT,
    primary_email TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE auth_identities (
    id TEXT PRIMARY KEY,
    user_jid TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT,
    email_verified INTEGER,
    raw_claims_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_login_at TEXT NOT NULL,
    UNIQUE(issuer, subject),
    FOREIGN KEY (user_jid) REFERENCES users(jid) ON DELETE CASCADE
);

CREATE INDEX idx_auth_identities_user_jid ON auth_identities(user_jid);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    user_jid TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    FOREIGN KEY (user_jid) REFERENCES users(jid) ON DELETE CASCADE
);

CREATE INDEX idx_sessions_user_jid ON sessions(user_jid);
CREATE INDEX idx_sessions_expires_at ON sessions(expires_at);

CREATE TABLE permission_tuples (
    id TEXT PRIMARY KEY,
    object_type TEXT NOT NULL,
    object_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    subject_relation TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    UNIQUE(object_type, object_id, relation, subject_type, subject_id, subject_relation)
);

CREATE INDEX idx_tuples_object ON permission_tuples(object_type, object_id);
CREATE INDEX idx_tuples_subject ON permission_tuples(subject_type, subject_id);
CREATE INDEX idx_tuples_relation ON permission_tuples(object_type, relation);
CREATE INDEX idx_tuples_check ON permission_tuples(object_type, object_id, relation, subject_type, subject_id);

CREATE TABLE native_users (
    id BIGSERIAL PRIMARY KEY,
    username TEXT NOT NULL,
    domain TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    salt TEXT NOT NULL,
    iterations INTEGER NOT NULL DEFAULT 4096,
    stored_key BYTEA NOT NULL,
    server_key BYTEA NOT NULL,
    email TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    UNIQUE(username, domain)
);

CREATE INDEX idx_native_users_username_domain ON native_users(username, domain);
CREATE INDEX idx_native_users_email ON native_users(email) WHERE email IS NOT NULL;

CREATE TABLE vcard_storage (
    jid TEXT PRIMARY KEY,
    vcard_xml TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT
);

CREATE TABLE upload_slots (
    id TEXT PRIMARY KEY,
    requester_jid TEXT NOT NULL,
    filename TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    content_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    storage_key TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    expires_at TEXT NOT NULL,
    uploaded_at TEXT
);

CREATE INDEX idx_upload_slots_requester ON upload_slots(requester_jid);
CREATE INDEX idx_upload_slots_expires ON upload_slots(expires_at) WHERE status = 'pending';
CREATE INDEX idx_upload_slots_status ON upload_slots(status);

CREATE TABLE roster_items (
    id BIGSERIAL PRIMARY KEY,
    user_jid TEXT NOT NULL,
    contact_jid TEXT NOT NULL,
    name TEXT,
    subscription TEXT NOT NULL DEFAULT 'none',
    ask TEXT,
    approved BOOLEAN NOT NULL DEFAULT FALSE,
    groups TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    UNIQUE(user_jid, contact_jid)
);

CREATE INDEX idx_roster_items_user ON roster_items(user_jid);
CREATE INDEX idx_roster_items_contact ON roster_items(contact_jid);
CREATE INDEX idx_roster_items_subscription ON roster_items(user_jid, subscription);

CREATE TABLE roster_versions (
    user_jid TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT
);

CREATE TABLE blocking_list (
    id BIGSERIAL PRIMARY KEY,
    user_jid TEXT NOT NULL,
    blocked_jid TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    UNIQUE(user_jid, blocked_jid)
);

CREATE INDEX idx_blocking_list_user ON blocking_list(user_jid);
CREATE INDEX idx_blocking_list_blocked ON blocking_list(blocked_jid);

CREATE TABLE private_xml_storage (
    jid TEXT NOT NULL,
    namespace TEXT NOT NULL,
    xml_content TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    PRIMARY KEY (jid, namespace)
);
"#;

/// `user_avatar_source` provenance table for the OIDC → PEP
/// avatar/FN bridge. The user-managed avatar guard reads this value
/// on `RemoveIfOidcOwned`: a row marked `'user'` keeps its picture
/// even when the IDP claim disappears.
///
/// Why a dedicated table rather than a column on `users`: native-auth
/// users (XEP-0077 / SCRAM, the test fixed-account fixture) only land
/// in `native_users`, not `users`. A column on `users` would force the
/// guard's write path to UPSERT a `users` row, which then collides
/// with the `users.username UNIQUE` constraint when an OIDC user
/// already owns the same `username`. A separate table keyed solely on
/// `xmpp_localpart` avoids that cross-row conflict surface entirely.
pub const V0002_USER_AVATAR_SOURCE: &str = r#"
CREATE TABLE user_avatar_source (
    xmpp_localpart TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('oidc', 'user')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

pub const V0002_USER_AVATAR_SOURCE_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS user_avatar_source (
    xmpp_localpart TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('oidc', 'user')),
    updated_at TEXT NOT NULL DEFAULT to_char(NOW() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
);
"#;

/// `user_avatar_fetch_state` — per-user attempt + outcome state for
/// the OIDC avatar fetcher's startup backfill. Keyed solely on
/// `xmpp_localpart` to avoid the same `users.username UNIQUE`
/// collision surface that drove `user_avatar_source` into a
/// dedicated table. `last_error` is `NULL` on success and otherwise
/// carries a `FetchError::kind()` value (`permanent_4xx`,
/// `mime_rejected`, `size_exceeded`, `ssrf_blocked`, etc.) — the
/// backfill consults it to throttle 4xx-style permanent failures
/// for a 24h cool-down without re-hammering the IDP every boot.
pub const V0003_USER_AVATAR_FETCH_STATE: &str = r#"
CREATE TABLE user_avatar_fetch_state (
    xmpp_localpart TEXT PRIMARY KEY,
    last_attempt_at TEXT NOT NULL,
    last_error TEXT,
    updated_at TEXT NOT NULL
);
"#;

pub const V0003_USER_AVATAR_FETCH_STATE_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS user_avatar_fetch_state (
    xmpp_localpart TEXT PRIMARY KEY,
    last_attempt_at TEXT NOT NULL,
    last_error TEXT,
    updated_at TEXT NOT NULL
);
"#;

/// Add `last_fetch_policy_digest` to the avatar throttle so a
/// fetch-policy widening (allowlist expansion, cap relaxation, etc.)
/// invalidates stale throttle rows for policy-dependent error
/// kinds without manual SQL. The column is `NULL` on existing rows
/// — `should_throttle` treats `NULL` as digest-mismatch and
/// retries policy-dependent failures on the next backfill round.
/// See `fetch::fetch_policy_digest` for the value being persisted
/// and `backfill::POLICY_DEPENDENT_KINDS` for the kinds invalidated.
pub const V0004_AVATAR_FETCH_STATE_POLICY_DIGEST: &str = r#"
ALTER TABLE user_avatar_fetch_state ADD COLUMN last_fetch_policy_digest TEXT;
"#;

/// Postgres variant uses `IF NOT EXISTS` so a re-run after a
/// crashed migration-bookkeeping insert (the `_migrations` row is
/// written outside the DDL transaction in the runner) is a clean
/// no-op rather than a hard failure on the second `ALTER TABLE`.
/// SQLite has no `IF NOT EXISTS` support on `ADD COLUMN`, so the
/// SQLite variant is not idempotent: a crash between the DDL
/// commit and the `_migrations` insert leaves the next boot
/// failing on "duplicate column" until a human clears the half-
/// applied state. This is a pre-existing migration-runner
/// fragility (the runner's incompatible-history reset only fires
/// on differing descriptions, not on missing-row + existing-column),
/// not something V0004 introduces or worsens — but the asymmetry
/// is worth flagging at the migration site.
pub const V0004_AVATAR_FETCH_STATE_POLICY_DIGEST_POSTGRES: &str = r#"
ALTER TABLE user_avatar_fetch_state ADD COLUMN IF NOT EXISTS last_fetch_policy_digest TEXT;
"#;

/// Delivery ledger for provider webhook ingress
/// (`/webhooks/providers/{provider_id}/{plugin_id}`). The unique
/// `(provider_id, delivery_id)` primary key deduplicates retried
/// deliveries from the provider (GitHub redelivers up to 8 times),
/// and the `status` index supports operator queries for stuck
/// `queued` rows whose dispatch task never ran to completion.
pub const V0005_PROVIDER_WEBHOOK_DELIVERY_LEDGER: &str = r#"
CREATE TABLE provider_webhook_deliveries (
    provider_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (provider_id, delivery_id)
);

CREATE INDEX idx_provider_webhook_deliveries_status
    ON provider_webhook_deliveries(status, attempts, created_at);
"#;

pub const V0005_PROVIDER_WEBHOOK_DELIVERY_LEDGER_POSTGRES: &str = r#"
CREATE TABLE provider_webhook_deliveries (
    provider_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    PRIMARY KEY (provider_id, delivery_id)
);

CREATE INDEX idx_provider_webhook_deliveries_status
    ON provider_webhook_deliveries(status, attempts, created_at);
"#;

/// Widen XEP-0363 upload slot sizes for Postgres. The request size is
/// accepted as `u64`, bounded by `WADDLE_MAX_UPLOAD_SIZE`, and then
/// stored as an `i64`; Postgres `INTEGER` would reject configured
/// maxima above int4 even though the Rust boundary accepts them.
pub const V0006_UPLOAD_SIZES_BIGINT: &str = r#"
SELECT 1;
"#;

pub const V0006_UPLOAD_SIZES_BIGINT_POSTGRES: &str = r#"
ALTER TABLE upload_slots
ALTER COLUMN size_bytes TYPE BIGINT;
"#;

pub const V0007_LINK_PREVIEW_MEDIA_REFS: &str = r#"
CREATE TABLE link_preview_media_refs (
    upload_slot_id TEXT NOT NULL,
    archive_jid TEXT NOT NULL,
    message_id TEXT NOT NULL,
    current_archive_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('current', 'unreferenced')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (upload_slot_id, archive_jid, message_id),
    FOREIGN KEY (upload_slot_id) REFERENCES upload_slots(id) ON DELETE CASCADE
);

CREATE INDEX idx_link_preview_media_refs_current
    ON link_preview_media_refs(upload_slot_id, state)
    WHERE state = 'current';
CREATE INDEX idx_link_preview_media_refs_message
    ON link_preview_media_refs(archive_jid, message_id);
"#;

pub const V0007_LINK_PREVIEW_MEDIA_REFS_POSTGRES: &str = r#"
CREATE TABLE link_preview_media_refs (
    upload_slot_id TEXT NOT NULL,
    archive_jid TEXT NOT NULL,
    message_id TEXT NOT NULL,
    current_archive_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('current', 'unreferenced')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    PRIMARY KEY (upload_slot_id, archive_jid, message_id),
    FOREIGN KEY (upload_slot_id) REFERENCES upload_slots(id) ON DELETE CASCADE
);

CREATE INDEX idx_link_preview_media_refs_current
    ON link_preview_media_refs(upload_slot_id, state)
    WHERE state = 'current';
CREATE INDEX idx_link_preview_media_refs_message
    ON link_preview_media_refs(archive_jid, message_id);
"#;

/// Repair databases where migration bookkeeping recorded V0005/V0007
/// without the tables being present. This is intentionally idempotent:
/// healthy databases no-op, while drifted databases recreate the missing
/// schema without requiring operators to rewrite `_migrations`.
pub const V0008_REPAIR_GLOBAL_SCHEMA_DRIFT: &str = r#"
CREATE TABLE IF NOT EXISTS provider_webhook_deliveries (
    provider_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (provider_id, delivery_id)
);

CREATE INDEX IF NOT EXISTS idx_provider_webhook_deliveries_status
    ON provider_webhook_deliveries(status, attempts, created_at);

CREATE TABLE IF NOT EXISTS link_preview_media_refs (
    upload_slot_id TEXT NOT NULL,
    archive_jid TEXT NOT NULL,
    message_id TEXT NOT NULL,
    current_archive_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('current', 'unreferenced')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (upload_slot_id, archive_jid, message_id),
    FOREIGN KEY (upload_slot_id) REFERENCES upload_slots(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_link_preview_media_refs_current
    ON link_preview_media_refs(upload_slot_id, state)
    WHERE state = 'current';
CREATE INDEX IF NOT EXISTS idx_link_preview_media_refs_message
    ON link_preview_media_refs(archive_jid, message_id);
"#;

pub const V0008_REPAIR_GLOBAL_SCHEMA_DRIFT_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS provider_webhook_deliveries (
    provider_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    PRIMARY KEY (provider_id, delivery_id)
);

CREATE INDEX IF NOT EXISTS idx_provider_webhook_deliveries_status
    ON provider_webhook_deliveries(status, attempts, created_at);

CREATE TABLE IF NOT EXISTS link_preview_media_refs (
    upload_slot_id TEXT NOT NULL,
    archive_jid TEXT NOT NULL,
    message_id TEXT NOT NULL,
    current_archive_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('current', 'unreferenced')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    PRIMARY KEY (upload_slot_id, archive_jid, message_id),
    FOREIGN KEY (upload_slot_id) REFERENCES upload_slots(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_link_preview_media_refs_current
    ON link_preview_media_refs(upload_slot_id, state)
    WHERE state = 'current';
CREATE INDEX IF NOT EXISTS idx_link_preview_media_refs_message
    ON link_preview_media_refs(archive_jid, message_id);
"#;

/// Durable OIDC/OAuth handshake state so any replica can serve the
/// callback / token / device endpoints (#1336). Entries are single-use
/// JSON payloads keyed by their opaque secret; `expires_at_ms` (unix
/// epoch milliseconds) exists purely for the janitor's SQL prune.
///
/// `IF NOT EXISTS` throughout: the migration runner has no cross-node
/// lock, so two replicas starting concurrently can both attempt this
/// DDL; idempotent statements make the loser a no-op instead of a
/// startup crash-loop.
pub const V0009_AUTH_HANDSHAKE_STATE: &str = r#"
CREATE TABLE IF NOT EXISTS pending_auth (
    state TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pending_auth_expires_at ON pending_auth(expires_at_ms);

CREATE TABLE IF NOT EXISTS device_auth (
    device_code TEXT PRIMARY KEY,
    user_code TEXT NOT NULL,
    status TEXT NOT NULL,
    payload TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_device_auth_user_code ON device_auth(user_code);
CREATE INDEX IF NOT EXISTS idx_device_auth_expires_at ON device_auth(expires_at_ms);

CREATE TABLE IF NOT EXISTS xmpp_auth_codes (
    code TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_xmpp_auth_codes_expires_at ON xmpp_auth_codes(expires_at_ms);
"#;

pub const V0009_AUTH_HANDSHAKE_STATE_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS pending_auth (
    state TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    expires_at_ms BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_pending_auth_expires_at ON pending_auth(expires_at_ms);

CREATE TABLE IF NOT EXISTS device_auth (
    device_code TEXT PRIMARY KEY,
    user_code TEXT NOT NULL,
    status TEXT NOT NULL,
    payload TEXT NOT NULL,
    expires_at_ms BIGINT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_device_auth_user_code ON device_auth(user_code);
CREATE INDEX IF NOT EXISTS idx_device_auth_expires_at ON device_auth(expires_at_ms);

CREATE TABLE IF NOT EXISTS xmpp_auth_codes (
    code TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    expires_at_ms BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_xmpp_auth_codes_expires_at ON xmpp_auth_codes(expires_at_ms);
"#;

/// Durable, non-secret reference metadata for XMPP SM resumption. The
/// context UUID is not a session credential; it is resolved against the
/// still-live session row and its exact version/epoch at resume time.
pub const V0010_AUTH_CONTEXT_REFERENCE: &str = r#"
ALTER TABLE sessions ADD COLUMN auth_context_id TEXT;
ALTER TABLE sessions ADD COLUMN auth_context_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE sessions ADD COLUMN principal_auth_epoch INTEGER NOT NULL DEFAULT 1;
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_auth_context
    ON sessions (auth_context_id);
"#;

pub const V0010_AUTH_CONTEXT_REFERENCE_POSTGRES: &str = r#"
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS auth_context_id UUID;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS auth_context_version BIGINT NOT NULL DEFAULT 1;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS principal_auth_epoch BIGINT NOT NULL DEFAULT 1;
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_auth_context
    ON sessions (auth_context_id);
"#;

/// Get all global migrations in order
pub fn all() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            description: "Hard-cut auth broker schema with roster pre-approval".to_string(),
            sql_sqlite: V0001_AUTH_BROKER_SCHEMA,
            sql_postgres: V0001_AUTH_BROKER_SCHEMA_POSTGRES,
        },
        Migration {
            version: 2,
            description:
                "Add user_avatar_source provenance table for OIDC user-managed avatar guard"
                    .to_string(),
            sql_sqlite: V0002_USER_AVATAR_SOURCE,
            sql_postgres: V0002_USER_AVATAR_SOURCE_POSTGRES,
        },
        Migration {
            version: 3,
            description: "Add user_avatar_fetch_state for startup-backfill throttle".to_string(),
            sql_sqlite: V0003_USER_AVATAR_FETCH_STATE,
            sql_postgres: V0003_USER_AVATAR_FETCH_STATE_POSTGRES,
        },
        Migration {
            version: 4,
            description: "Add last_fetch_policy_digest column for deploy-time throttle reset"
                .to_string(),
            sql_sqlite: V0004_AVATAR_FETCH_STATE_POLICY_DIGEST,
            sql_postgres: V0004_AVATAR_FETCH_STATE_POLICY_DIGEST_POSTGRES,
        },
        Migration {
            version: 5,
            description: "Provider webhook delivery ledger".to_string(),
            sql_sqlite: V0005_PROVIDER_WEBHOOK_DELIVERY_LEDGER,
            sql_postgres: V0005_PROVIDER_WEBHOOK_DELIVERY_LEDGER_POSTGRES,
        },
        Migration {
            version: 6,
            description: "Widen upload slot sizes to bigint on Postgres".to_string(),
            sql_sqlite: V0006_UPLOAD_SIZES_BIGINT,
            sql_postgres: V0006_UPLOAD_SIZES_BIGINT_POSTGRES,
        },
        Migration {
            version: 7,
            description: "Track current link preview media references".to_string(),
            sql_sqlite: V0007_LINK_PREVIEW_MEDIA_REFS,
            sql_postgres: V0007_LINK_PREVIEW_MEDIA_REFS_POSTGRES,
        },
        Migration {
            version: 8,
            description: "Repair missing global tables after migration drift".to_string(),
            sql_sqlite: V0008_REPAIR_GLOBAL_SCHEMA_DRIFT,
            sql_postgres: V0008_REPAIR_GLOBAL_SCHEMA_DRIFT_POSTGRES,
        },
        Migration {
            version: 9,
            description: "Durable OIDC/OAuth handshake state shared across replicas".to_string(),
            sql_sqlite: V0009_AUTH_HANDSHAKE_STATE,
            sql_postgres: V0009_AUTH_HANDSHAKE_STATE_POSTGRES,
        },
        Migration {
            version: 10,
            description: "Durable non-secret auth-context references for SM resume".to_string(),
            sql_sqlite: V0010_AUTH_CONTEXT_REFERENCE,
            sql_postgres: V0010_AUTH_CONTEXT_REFERENCE_POSTGRES,
        },
    ]
}
