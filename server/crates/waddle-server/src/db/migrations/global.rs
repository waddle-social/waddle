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
DROP TABLE IF EXISTS roster_items;
DROP TABLE IF EXISTS roster_versions;
DROP TABLE IF EXISTS blocking_list;
DROP TABLE IF EXISTS private_xml_storage;

CREATE TABLE users (
    id TEXT PRIMARY KEY,
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
    user_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT,
    email_verified INTEGER,
    raw_claims_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_login_at TEXT NOT NULL,
    UNIQUE(issuer, subject),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_auth_identities_user_id ON auth_identities(user_id);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_sessions_user_id ON sessions(user_id);
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
DROP TABLE IF EXISTS roster_items CASCADE;
DROP TABLE IF EXISTS roster_versions CASCADE;
DROP TABLE IF EXISTS blocking_list CASCADE;
DROP TABLE IF EXISTS private_xml_storage CASCADE;

CREATE TABLE users (
    id TEXT PRIMARY KEY,
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
    user_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT,
    email_verified INTEGER,
    raw_claims_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_login_at TEXT NOT NULL,
    UNIQUE(issuer, subject),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_auth_identities_user_id ON auth_identities(user_id);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_sessions_user_id ON sessions(user_id);
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
    size_bytes INTEGER NOT NULL,
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
CREATE TABLE user_avatar_source (
    xmpp_localpart TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK (source IN ('oidc', 'user')),
    updated_at TEXT NOT NULL DEFAULT to_char(NOW(), 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
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
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

pub const V0003_USER_AVATAR_FETCH_STATE_POSTGRES: &str = r#"
CREATE TABLE user_avatar_fetch_state (
    xmpp_localpart TEXT PRIMARY KEY,
    last_attempt_at TEXT NOT NULL,
    last_error TEXT,
    updated_at TEXT NOT NULL DEFAULT to_char(NOW(), 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
);
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
    ]
}
