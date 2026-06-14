use super::Migration;

/// Hard-cut per-waddle schema with bare-JID user principals.
pub const V0001_SCHEMA: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS attachments;
DROP TABLE IF EXISTS reactions;
DROP TABLE IF EXISTS messages;
DROP TABLE IF EXISTS channels;

CREATE TABLE channels (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    channel_type TEXT NOT NULL DEFAULT 'text',
    position INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_channels_position ON channels(position);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    author_jid TEXT NOT NULL,
    content TEXT,
    reply_to_id TEXT,
    thread_id TEXT,
    flags INTEGER NOT NULL DEFAULT 0,
    edited_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT,
    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_channel_id ON messages(channel_id);
CREATE INDEX idx_messages_author_jid ON messages(author_jid);
CREATE INDEX idx_messages_created_at ON messages(created_at);
CREATE INDEX idx_messages_reply_to_id ON messages(reply_to_id);
CREATE INDEX idx_messages_thread ON messages(thread_id, created_at);
CREATE INDEX idx_messages_channel_created ON messages(channel_id, created_at DESC);
CREATE INDEX idx_messages_expires ON messages(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE reactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL,
    user_jid TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(message_id, user_jid, emoji),
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_reactions_message_id ON reactions(message_id);

CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    storage_key TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachments_message_id ON attachments(message_id);

PRAGMA foreign_keys = ON;
"#;

/// Hard-cut per-waddle schema with bare-JID user principals — Postgres dialect.
pub const V0001_SCHEMA_POSTGRES: &str = r#"
DROP TABLE IF EXISTS attachments CASCADE;
DROP TABLE IF EXISTS reactions CASCADE;
DROP TABLE IF EXISTS messages CASCADE;
DROP TABLE IF EXISTS channels CASCADE;

CREATE TABLE channels (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    channel_type TEXT NOT NULL DEFAULT 'text',
    position INTEGER NOT NULL DEFAULT 0,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT
);

CREATE INDEX idx_channels_position ON channels(position);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    author_jid TEXT NOT NULL,
    content TEXT,
    reply_to_id TEXT,
    thread_id TEXT,
    flags INTEGER NOT NULL DEFAULT 0,
    edited_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    expires_at TEXT,
    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_channel_id ON messages(channel_id);
CREATE INDEX idx_messages_author_jid ON messages(author_jid);
CREATE INDEX idx_messages_created_at ON messages(created_at);
CREATE INDEX idx_messages_reply_to_id ON messages(reply_to_id);
CREATE INDEX idx_messages_thread ON messages(thread_id, created_at);
CREATE INDEX idx_messages_channel_created ON messages(channel_id, created_at DESC);
CREATE INDEX idx_messages_expires ON messages(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE reactions (
    id BIGSERIAL PRIMARY KEY,
    message_id TEXT NOT NULL,
    user_jid TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    UNIQUE(message_id, user_jid, emoji),
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_reactions_message_id ON reactions(message_id);

CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    storage_key TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachments_message_id ON attachments(message_id);
"#;

/// Persist a room's pin permission policy so disco-info on a dormant room
/// (no live actor) can advertise the truth instead of the default.
pub const V1002_ADD_CHANNEL_PIN_PERMISSION: &str = r#"
ALTER TABLE channels
ADD COLUMN pin_permission TEXT NOT NULL DEFAULT 'admins-only';
"#;

/// Postgres variant is idempotent because prod was manually hot-patched before
/// this migration existed.
pub const V1002_ADD_CHANNEL_PIN_PERMISSION_POSTGRES: &str = r#"
ALTER TABLE channels
ADD COLUMN IF NOT EXISTS pin_permission TEXT NOT NULL DEFAULT 'admins-only';
"#;

/// Widen attachment sizes for Postgres. SQLite `INTEGER` already
/// stores dynamic-width integer values, while Postgres `INTEGER` is
/// int4 and can reject valid Rust `i64` sizes.
pub const V1003_ATTACHMENT_SIZES_BIGINT: &str = r#"
SELECT 1;
"#;

pub const V1003_ATTACHMENT_SIZES_BIGINT_POSTGRES: &str = r#"
ALTER TABLE attachments
ALTER COLUMN size_bytes TYPE BIGINT;
"#;

/// Persist the XEP-0045 members-only room policy used by admin-created
/// channels. The room registry is a runtime materialization; restart recovery
/// rebuilds rooms from the channel catalog and needs this field to preserve
/// membership policy.
pub const V1004_ADD_CHANNEL_MEMBERS_ONLY: &str = r#"
ALTER TABLE channels
ADD COLUMN members_only INTEGER NOT NULL DEFAULT 1;
"#;

pub const V1004_ADD_CHANNEL_MEMBERS_ONLY_POSTGRES: &str = r#"
ALTER TABLE channels
ADD COLUMN IF NOT EXISTS members_only INTEGER NOT NULL DEFAULT 1;
"#;

/// Persist the independent XEP-0045 public-room discovery visibility bit.
/// Public visibility is not the same as membership policy: a room can be
/// visible in MUC disco while still requiring membership to enter.
pub const V1005_ADD_CHANNEL_PUBLIC_ROOM: &str = r#"
ALTER TABLE channels
ADD COLUMN public_room INTEGER NOT NULL DEFAULT 1;
"#;

pub const V1005_ADD_CHANNEL_PUBLIC_ROOM_POSTGRES: &str = r#"
ALTER TABLE channels
ADD COLUMN IF NOT EXISTS public_room INTEGER NOT NULL DEFAULT 1;
"#;

/// Per-member MAM visibility boundary for group-DM mediated invites.
/// A `NULL` boundary means full history access; a timestamp boundary means
/// the member may only read archived room messages created at or after it.
pub const V1006_ADD_GROUP_DM_ARCHIVE_BOUNDARIES: &str = r#"
CREATE TABLE IF NOT EXISTS group_dm_archive_boundaries (
    room_jid TEXT NOT NULL,
    member_jid TEXT NOT NULL,
    visible_after TEXT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (room_jid, member_jid)
);
"#;

pub const V1006_ADD_GROUP_DM_ARCHIVE_BOUNDARIES_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS group_dm_archive_boundaries (
    room_jid TEXT NOT NULL,
    member_jid TEXT NOT NULL,
    visible_after TEXT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (room_jid, member_jid)
);
"#;

/// Get all waddle schema migrations in order.
///
/// Versions are intentionally offset from global migrations so a single
/// database can safely apply both sets without migration history collisions.
pub fn all() -> Vec<Migration> {
    vec![
        Migration {
            version: 1001,
            description: "Hard-cut per-waddle schema with bare-JID principals".to_string(),
            sql_sqlite: V0001_SCHEMA,
            sql_postgres: V0001_SCHEMA_POSTGRES,
        },
        Migration {
            version: 1002,
            description: "Add channel pin permission policy".to_string(),
            sql_sqlite: V1002_ADD_CHANNEL_PIN_PERMISSION,
            sql_postgres: V1002_ADD_CHANNEL_PIN_PERMISSION_POSTGRES,
        },
        Migration {
            version: 1003,
            description: "Widen attachment sizes to bigint on Postgres".to_string(),
            sql_sqlite: V1003_ATTACHMENT_SIZES_BIGINT,
            sql_postgres: V1003_ATTACHMENT_SIZES_BIGINT_POSTGRES,
        },
        Migration {
            version: 1004,
            description: "Persist channel members-only policy".to_string(),
            sql_sqlite: V1004_ADD_CHANNEL_MEMBERS_ONLY,
            sql_postgres: V1004_ADD_CHANNEL_MEMBERS_ONLY_POSTGRES,
        },
        Migration {
            version: 1005,
            description: "Persist channel public-room discovery visibility".to_string(),
            sql_sqlite: V1005_ADD_CHANNEL_PUBLIC_ROOM,
            sql_postgres: V1005_ADD_CHANNEL_PUBLIC_ROOM_POSTGRES,
        },
        Migration {
            version: 1006,
            description: "Persist group-DM archive visibility boundaries".to_string(),
            sql_sqlite: V1006_ADD_GROUP_DM_ARCHIVE_BOUNDARIES,
            sql_postgres: V1006_ADD_GROUP_DM_ARCHIVE_BOUNDARIES_POSTGRES,
        },
    ]
}
