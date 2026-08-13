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

/// Outstanding mediated-invite ledger (XEP-0045 §7.8.2, #1264): the
/// room only forwards a `<decline/>` from a JID it actually invited,
/// and the recorded inviter is who the decline must reach (durably,
/// even when they are offline). Consumed on decline; wiped on room
/// destroy.
pub const V1007_ADD_MUC_PENDING_INVITES: &str = r#"
CREATE TABLE IF NOT EXISTS muc_pending_invites (
    room_jid TEXT NOT NULL,
    invitee_jid TEXT NOT NULL,
    inviter_jid TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (room_jid, invitee_jid, inviter_jid)
);
"#;

pub const V1007_ADD_MUC_PENDING_INVITES_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS muc_pending_invites (
    room_jid TEXT NOT NULL,
    invitee_jid TEXT NOT NULL,
    inviter_jid TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (room_jid, invitee_jid, inviter_jid)
);
"#;

/// PostgreSQL-only ingress foundation tables. SQLite keeps its migration
/// ledger in sync with a no-op because the ingress substrate is Postgres-only.
pub const V1008_INGRESS_FOUNDATION_TABLES: &str = r#"
SELECT 1;
"#;

pub const V1008_INGRESS_FOUNDATION_TABLES_POSTGRES: &str = r#"
CREATE TABLE ingress_protocol_epoch (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    epoch BIGINT NOT NULL DEFAULT 0 CHECK (epoch BETWEEN 0 AND 4294967295),
    activated_at TIMESTAMPTZ NULL,
    lineage_uuid UUID NULL,
    CHECK ((epoch = 0) = (activated_at IS NULL AND lineage_uuid IS NULL))
);
INSERT INTO ingress_protocol_epoch (id, epoch) VALUES (1, 0);

CREATE TABLE ingress_messages (
    message_key UUID PRIMARY KEY,
    digest_version INTEGER NOT NULL CHECK (digest_version = 1),
    digest BYTEA NOT NULL CHECK (octet_length(digest) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    terminal_at TIMESTAMPTZ NULL,
    row_revision NUMERIC(20,0) NOT NULL DEFAULT 0
        CHECK (row_revision >= 0 AND row_revision <= 18446744073709551615)
);
CREATE INDEX ingress_messages_terminal_at_idx
    ON ingress_messages (terminal_at) WHERE terminal_at IS NOT NULL;

-- Alias uniqueness rides a fixed-width SHA-256 of the length-prefixed
-- (sender, target kind, target, origin-id) canonical encoding, computed by
-- the substrate store.  A composite B-tree key over the raw columns can
-- exceed PostgreSQL's index-row limit at maximum JID + origin-id lengths;
-- the raw columns stay for audit and GC but carry no index.
CREATE TABLE ingress_origin_aliases (
    alias_key_hash BYTEA PRIMARY KEY CHECK (octet_length(alias_key_hash) = 32),
    sender_bare_jid TEXT NOT NULL
        CHECK (sender_bare_jid <> '' AND octet_length(sender_bare_jid) <= 3071),
    target_kind INTEGER NOT NULL CHECK (target_kind IN (0, 1, 2)),
    target_jid TEXT NOT NULL DEFAULT '' CHECK (octet_length(target_jid) <= 3071),
    origin_id TEXT NOT NULL CHECK (origin_id <> '' AND octet_length(origin_id) <= 1024),
    message_key UUID NOT NULL REFERENCES ingress_messages (message_key),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((target_kind = 0) = (target_jid = ''))
);
CREATE INDEX ingress_origin_aliases_message_key_idx ON ingress_origin_aliases (message_key);

CREATE TABLE ingress_sm_refs (
    sm_ingress_id UUID NOT NULL,
    ingress_ordinal NUMERIC(20,0) NOT NULL
        CHECK (ingress_ordinal >= 1 AND ingress_ordinal <= 18446744073709551615),
    message_key UUID NOT NULL REFERENCES ingress_messages (message_key),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (sm_ingress_id, ingress_ordinal)
);
CREATE INDEX ingress_sm_refs_message_key_idx ON ingress_sm_refs (message_key);

CREATE TABLE ingress_deliveries (
    delivery_key UUID PRIMARY KEY,
    message_key UUID NOT NULL REFERENCES ingress_messages (message_key),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    row_revision NUMERIC(20,0) NOT NULL DEFAULT 0
        CHECK (row_revision >= 0 AND row_revision <= 18446744073709551615)
);
CREATE INDEX ingress_deliveries_message_key_idx ON ingress_deliveries (message_key);
"#;

/// PostgreSQL-only inert ingress epoch guards. SQLite keeps its migration
/// ledger in sync with a no-op because the ingress substrate is Postgres-only.
pub const V1009_INERT_INGRESS_EPOCH_GUARDS: &str = r#"
SELECT 1;
"#;

pub const V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES: &str = r#"
CREATE FUNCTION waddle_ingress_epoch_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
DECLARE
    live_epoch BIGINT;
    tx_epoch TEXT;
    tx_xid TEXT;
BEGIN
    EXECUTE pg_catalog.format(
        'SELECT epoch FROM %I.ingress_protocol_epoch WHERE id = 1 FOR SHARE',
        TG_TABLE_SCHEMA
    ) INTO live_epoch;
    IF live_epoch IS NULL THEN
        RAISE EXCEPTION 'waddle: ingress_protocol_epoch row missing; refusing % on %.%',
            TG_OP, TG_TABLE_SCHEMA, TG_TABLE_NAME;
    END IF;
    IF live_epoch = 0 THEN
        RETURN NULL;
    END IF;
    tx_epoch := current_setting('waddle.protocol_epoch', true);
    tx_xid := current_setting('waddle.protocol_epoch_xid', true);
    IF tx_epoch IS NULL OR tx_epoch <> live_epoch::text
       OR tx_xid IS NULL OR tx_xid <> pg_current_xact_id()::text THEN
        RAISE EXCEPTION 'waddle: % on %.% requires a transaction-local epoch proof (SET LOCAL waddle.protocol_epoch = %; SELECT set_config(''waddle.protocol_epoch_xid'', pg_current_xact_id()::text, true))',
            TG_OP, TG_TABLE_SCHEMA, TG_TABLE_NAME, live_epoch;
    END IF;
    RETURN NULL;
END $$;

-- TRUNCATE acquires the table's ACCESS EXCLUSIVE lock BEFORE its trigger
-- runs, so a truncate trigger that then requested the epoch row would invert
-- the global epoch-first lock order and could deadlock against row-wise GC
-- with an activation queued between them.  TRUNCATE is never a sanctioned
-- operation on the protected correctness tables (deletes are row-wise and
-- epoch-proven), so it is rejected unconditionally without taking any lock.
CREATE FUNCTION waddle_ingress_truncate_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    RAISE EXCEPTION 'waddle: TRUNCATE is not permitted on %.%',
        TG_TABLE_SCHEMA, TG_TABLE_NAME;
END $$;

CREATE TRIGGER ingress_messages_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_messages
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_messages_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_messages
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_messages ENABLE ALWAYS TRIGGER ingress_messages_epoch_guard_dml;
ALTER TABLE ingress_messages ENABLE ALWAYS TRIGGER ingress_messages_epoch_guard_truncate;

CREATE TRIGGER ingress_origin_aliases_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_origin_aliases
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_origin_aliases_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_origin_aliases
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_origin_aliases ENABLE ALWAYS TRIGGER ingress_origin_aliases_epoch_guard_dml;
ALTER TABLE ingress_origin_aliases ENABLE ALWAYS TRIGGER ingress_origin_aliases_epoch_guard_truncate;

CREATE TRIGGER ingress_sm_refs_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_sm_refs
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_sm_refs_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_sm_refs
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_sm_refs ENABLE ALWAYS TRIGGER ingress_sm_refs_epoch_guard_dml;
ALTER TABLE ingress_sm_refs ENABLE ALWAYS TRIGGER ingress_sm_refs_epoch_guard_truncate;

CREATE TRIGGER ingress_deliveries_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_deliveries
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_deliveries_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_deliveries
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_deliveries ENABLE ALWAYS TRIGGER ingress_deliveries_epoch_guard_dml;
ALTER TABLE ingress_deliveries ENABLE ALWAYS TRIGGER ingress_deliveries_epoch_guard_truncate;

CREATE FUNCTION waddle_ingress_protocol_epoch_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.epoch <> 0 AND (NEW.activated_at IS NULL OR NEW.lineage_uuid IS NULL) THEN
            RAISE EXCEPTION 'waddle: nonzero ingress protocol epochs require activation metadata';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE' THEN
        -- One advance per transaction: each row version validates +1
        -- independently, so without this transaction-local sentinel a
        -- duplicated activation statement would commit a two-epoch jump
        -- that no deployment ever observed.
        IF current_setting('waddle.protocol_epoch_advance_xid', true)
               = pg_current_xact_id()::text THEN
            RAISE EXCEPTION 'waddle: only one ingress protocol epoch advance per transaction';
        END IF;
        IF NEW.epoch <> OLD.epoch + 1 THEN
            RAISE EXCEPTION 'waddle: ingress protocol epoch must advance exactly one step';
        END IF;
        IF NEW.activated_at IS NULL OR NEW.lineage_uuid IS NULL THEN
            RAISE EXCEPTION 'waddle: nonzero ingress protocol epochs require activation metadata';
        END IF;
        PERFORM set_config('waddle.protocol_epoch_advance_xid', pg_current_xact_id()::text, true);
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'waddle: refusing % on ingress_protocol_epoch', TG_OP;
END $$;

CREATE TRIGGER ingress_protocol_epoch_guard_row
BEFORE INSERT OR UPDATE OR DELETE ON ingress_protocol_epoch
FOR EACH ROW EXECUTE FUNCTION waddle_ingress_protocol_epoch_guard();
CREATE TRIGGER ingress_protocol_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_protocol_epoch
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_protocol_epoch_guard();
ALTER TABLE ingress_protocol_epoch ENABLE ALWAYS TRIGGER ingress_protocol_epoch_guard_row;
ALTER TABLE ingress_protocol_epoch ENABLE ALWAYS TRIGGER ingress_protocol_epoch_guard_truncate;

CREATE TABLE ingress_epoch_guard_manifest (
    table_name TEXT PRIMARY KEY
);
INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES
    ('ingress_messages'),
    ('ingress_origin_aliases'),
    ('ingress_sm_refs'),
    ('ingress_deliveries');

CREATE FUNCTION waddle_ingress_epoch_guard_manifest_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'waddle: ingress epoch guard manifest is append-only';
END $$;

CREATE TRIGGER ingress_epoch_guard_manifest_append_only_row
BEFORE INSERT OR UPDATE OR DELETE ON ingress_epoch_guard_manifest
FOR EACH ROW EXECUTE FUNCTION waddle_ingress_epoch_guard_manifest_append_only();
CREATE TRIGGER ingress_epoch_guard_manifest_append_only_truncate
BEFORE TRUNCATE ON ingress_epoch_guard_manifest
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard_manifest_append_only();
ALTER TABLE ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_row;
ALTER TABLE ingress_epoch_guard_manifest ENABLE ALWAYS TRIGGER ingress_epoch_guard_manifest_append_only_truncate;
"#;

/// PostgreSQL-only shadow ingress durable surface. SQLite keeps its ledger in
/// sync because these tables are only used by the clustered ingress UoW.
pub const V1010_SHADOW_INGRESS_SURFACE: &str = r#"
SELECT 1;
"#;

pub const V1010_SHADOW_INGRESS_SURFACE_POSTGRES: &str = r#"
CREATE TABLE ingress_sm_streams (
    sm_ingress_id UUID PRIMARY KEY,
    stream_id TEXT NOT NULL UNIQUE CHECK (stream_id <> '' AND length(stream_id) <= 3071),
    handled_ordinal NUMERIC(20,0) NOT NULL DEFAULT 0
        CHECK (handled_ordinal >= 0 AND handled_ordinal <= 18446744073709551615),
    row_revision NUMERIC(20,0) NOT NULL DEFAULT 0
        CHECK (row_revision >= 0 AND row_revision <= 18446744073709551615),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ingress_effect_intents (
    message_key UUID NOT NULL REFERENCES ingress_messages (message_key) ON DELETE CASCADE,
    effect_ordinal NUMERIC(20,0) NOT NULL
        CHECK (effect_ordinal >= 0 AND effect_ordinal <= 18446744073709551615),
    kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 15),
    semantic_identity_hash BYTEA NOT NULL CHECK (octet_length(semantic_identity_hash) = 32),
    payload_version INTEGER NOT NULL CHECK (payload_version = 1),
    payload BYTEA NOT NULL CHECK (octet_length(payload) <= 65536),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (message_key, kind, semantic_identity_hash)
);

CREATE TRIGGER ingress_sm_streams_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_sm_streams
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_sm_streams_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_sm_streams
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_sm_streams ENABLE ALWAYS TRIGGER ingress_sm_streams_epoch_guard_dml;
ALTER TABLE ingress_sm_streams ENABLE ALWAYS TRIGGER ingress_sm_streams_epoch_guard_truncate;

CREATE TRIGGER ingress_effect_intents_epoch_guard_dml
BEFORE INSERT OR UPDATE OR DELETE ON ingress_effect_intents
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_epoch_guard();
CREATE TRIGGER ingress_effect_intents_epoch_guard_truncate
BEFORE TRUNCATE ON ingress_effect_intents
FOR EACH STATEMENT EXECUTE FUNCTION waddle_ingress_truncate_guard();
ALTER TABLE ingress_effect_intents ENABLE ALWAYS TRIGGER ingress_effect_intents_epoch_guard_dml;
ALTER TABLE ingress_effect_intents ENABLE ALWAYS TRIGGER ingress_effect_intents_epoch_guard_truncate;

INSERT INTO ingress_epoch_guard_manifest (table_name) VALUES
    ('ingress_sm_streams'),
    ('ingress_effect_intents');
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
        Migration {
            version: 1007,
            description: "Persist outstanding MUC mediated invitations".to_string(),
            sql_sqlite: V1007_ADD_MUC_PENDING_INVITES,
            sql_postgres: V1007_ADD_MUC_PENDING_INVITES_POSTGRES,
        },
        Migration {
            version: 1008,
            description: "Create PostgreSQL ingress foundation tables".to_string(),
            sql_sqlite: V1008_INGRESS_FOUNDATION_TABLES,
            sql_postgres: V1008_INGRESS_FOUNDATION_TABLES_POSTGRES,
        },
        Migration {
            version: 1009,
            description: "Add inert PostgreSQL ingress epoch guards".to_string(),
            sql_sqlite: V1009_INERT_INGRESS_EPOCH_GUARDS,
            sql_postgres: V1009_INERT_INGRESS_EPOCH_GUARDS_POSTGRES,
        },
        Migration {
            version: 1010,
            description: "Add shadow ingress streams and effect intents".to_string(),
            sql_sqlite: V1010_SHADOW_INGRESS_SURFACE,
            sql_postgres: V1010_SHADOW_INGRESS_SURFACE_POSTGRES,
        },
    ]
}
