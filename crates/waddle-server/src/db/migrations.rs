//! Database migration system for Waddle Server
//!
//! This module provides:
//! - Compile-time embedded SQL migrations
//! - Version tracking via a migrations table
//! - Automatic migration on database initialization
//!
//! # Migration Naming Convention
//!
//! Migration files should be named: `NNNN_description.sql`
//! Where NNNN is a zero-padded version number (e.g., 0001, 0002).

use super::Database;
use super::DatabaseError;
use tracing::{debug, info, instrument};

/// Represents a single database migration
#[derive(Debug, Clone)]
pub struct Migration {
    /// Version number (must be unique and incrementing)
    pub version: i64,
    /// Description of what this migration does
    pub description: String,
    /// SQL to execute for the migration
    pub sql: &'static str,
}

impl Migration {
    /// Create a new migration
    /// Note: description parameter is unused in const fn as String::new() is const but
    /// String::from() is not. The description is set at runtime in the all() functions.
    #[allow(dead_code)]
    pub const fn new(version: i64, _description: &'static str, sql: &'static str) -> Self {
        Self {
            version,
            description: String::new(), // Will be set at runtime
            sql,
        }
    }
}

/// Global database migrations (users, sessions, waddle registry)
pub mod global {
    use super::Migration;

    /// Initial global schema - users, sessions, and waddle registry
    pub const V0001_INITIAL_SCHEMA: &str = r#"
-- Users table (linked to ATProto DID)
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    did TEXT NOT NULL UNIQUE,              -- ATProto DID (decentralized identifier)
    handle TEXT,                            -- ATProto handle (e.g., user.bsky.social)
    display_name TEXT,                      -- Display name
    avatar_url TEXT,                        -- Avatar URL
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Index for quick DID lookups
CREATE INDEX IF NOT EXISTS idx_users_did ON users(did);
CREATE INDEX IF NOT EXISTS idx_users_handle ON users(handle);

-- Sessions table for authentication
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,                    -- Session ID (UUID)
    user_id INTEGER NOT NULL,               -- Reference to users table
    access_token TEXT,                      -- ATProto access token (encrypted)
    refresh_token TEXT,                     -- ATProto refresh token (encrypted)
    expires_at TEXT,                        -- Session expiration
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions(expires_at);

-- Waddles (communities) registry
CREATE TABLE IF NOT EXISTS waddles (
    id TEXT PRIMARY KEY,                    -- Waddle ID (UUID)
    name TEXT NOT NULL,                     -- Waddle name
    description TEXT,                       -- Waddle description
    owner_id INTEGER NOT NULL,              -- Owner user ID
    icon_url TEXT,                          -- Waddle icon URL
    is_public INTEGER NOT NULL DEFAULT 1,  -- Whether the waddle is publicly discoverable
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (owner_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_waddles_owner_id ON waddles(owner_id);
CREATE INDEX IF NOT EXISTS idx_waddles_is_public ON waddles(is_public);

-- Waddle memberships
CREATE TABLE IF NOT EXISTS waddle_members (
    waddle_id TEXT NOT NULL,
    user_id INTEGER NOT NULL,
    role TEXT NOT NULL DEFAULT 'member',   -- 'owner', 'admin', 'moderator', 'member'
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (waddle_id, user_id),
    FOREIGN KEY (waddle_id) REFERENCES waddles(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_waddle_members_user_id ON waddle_members(user_id);
"#;

    /// Migration to add token_endpoint and pds_url columns to sessions table
    pub const V0002_ADD_TOKEN_ENDPOINT: &str = r#"
-- Add token_endpoint and pds_url columns for token refresh support
ALTER TABLE sessions ADD COLUMN token_endpoint TEXT;
ALTER TABLE sessions ADD COLUMN pds_url TEXT;
"#;

    /// Migration to add permission_tuples table for Zanzibar-style permissions
    pub const V0003_PERMISSION_TUPLES: &str = r#"
-- Permission tuples table for Zanzibar-inspired ReBAC
-- Stores relationships in format: object#relation@subject
CREATE TABLE IF NOT EXISTS permission_tuples (
    id TEXT PRIMARY KEY,
    object_type TEXT NOT NULL,      -- waddle, channel, message, dm, role
    object_id TEXT NOT NULL,
    relation TEXT NOT NULL,         -- owner, admin, member, viewer, etc.
    subject_type TEXT NOT NULL,     -- user, waddle, role
    subject_id TEXT NOT NULL,
    subject_relation TEXT,          -- for set-based subjects (e.g., waddle:abc#member)
    created_at TEXT NOT NULL DEFAULT (datetime('now')),

    UNIQUE(object_type, object_id, relation, subject_type, subject_id, subject_relation)
);

-- Index for looking up all relations on an object (e.g., "who has access to channel:general?")
CREATE INDEX IF NOT EXISTS idx_tuples_object ON permission_tuples(object_type, object_id);

-- Index for looking up all objects a subject has access to (e.g., "what can user:alice access?")
CREATE INDEX IF NOT EXISTS idx_tuples_subject ON permission_tuples(subject_type, subject_id);

-- Index for looking up specific relation types (e.g., "all owners of waddles")
CREATE INDEX IF NOT EXISTS idx_tuples_relation ON permission_tuples(object_type, relation);

-- Composite index for permission checks (most common query pattern)
CREATE INDEX IF NOT EXISTS idx_tuples_check ON permission_tuples(object_type, object_id, relation, subject_type, subject_id);
"#;

    /// Migration to add native_users table for XEP-0077 In-Band Registration
    pub const V0004_NATIVE_USERS: &str = r#"
-- Native XMPP users table for XEP-0077 In-Band Registration
-- These users authenticate via SCRAM-SHA-256 (native JID) rather than ATProto OAuth
CREATE TABLE IF NOT EXISTS native_users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,                 -- Local part of JID (e.g., "alice" in alice@domain.com)
    domain TEXT NOT NULL,                   -- Domain part of JID (e.g., "domain.com")
    password_hash TEXT NOT NULL,            -- Argon2id hash of the password
    salt TEXT NOT NULL,                     -- SCRAM salt (base64 encoded, used for PBKDF2)
    iterations INTEGER NOT NULL DEFAULT 4096, -- PBKDF2 iteration count
    stored_key BLOB NOT NULL,               -- SCRAM StoredKey = H(ClientKey)
    server_key BLOB NOT NULL,               -- SCRAM ServerKey = HMAC(SaltedPassword, "Server Key")
    email TEXT,                             -- Optional email for recovery
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(username, domain)
);

-- Index for username lookup during authentication
CREATE INDEX IF NOT EXISTS idx_native_users_username_domain ON native_users(username, domain);

-- Index for email (for recovery features)
CREATE INDEX IF NOT EXISTS idx_native_users_email ON native_users(email) WHERE email IS NOT NULL;
"#;

    /// Get all global migrations in order
    pub fn all() -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                description: "Initial global schema".to_string(),
                sql: V0001_INITIAL_SCHEMA,
            },
            Migration {
                version: 2,
                description: "Add token_endpoint and pds_url to sessions".to_string(),
                sql: V0002_ADD_TOKEN_ENDPOINT,
            },
            Migration {
                version: 3,
                description: "Add permission_tuples table for Zanzibar-style ReBAC".to_string(),
                sql: V0003_PERMISSION_TUPLES,
            },
            Migration {
                version: 4,
                description: "Add native_users table for XEP-0077 In-Band Registration".to_string(),
                sql: V0004_NATIVE_USERS,
            },
        ]
    }
}

/// Per-Waddle database migrations (channels, messages)
pub mod waddle {
    use super::Migration;

    /// Initial per-waddle schema - channels and messages
    pub const V0001_INITIAL_SCHEMA: &str = r#"
-- Channels within a waddle
CREATE TABLE IF NOT EXISTS channels (
    id TEXT PRIMARY KEY,                    -- Channel ID (UUID)
    name TEXT NOT NULL,                     -- Channel name (e.g., "general")
    description TEXT,                       -- Channel description
    channel_type TEXT NOT NULL DEFAULT 'text', -- 'text', 'voice', 'announcement'
    position INTEGER NOT NULL DEFAULT 0,   -- Display order
    is_default INTEGER NOT NULL DEFAULT 0, -- Is this the default channel?
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_channels_position ON channels(position);

-- Messages in channels
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,                    -- Message ID (UUID)
    channel_id TEXT NOT NULL,               -- Channel this message belongs to
    author_did TEXT NOT NULL,               -- Author's ATProto DID
    content TEXT NOT NULL,                  -- Message content (may be encrypted)
    reply_to_id TEXT,                       -- ID of message being replied to
    is_edited INTEGER NOT NULL DEFAULT 0,  -- Has this message been edited?
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id);
CREATE INDEX IF NOT EXISTS idx_messages_author_did ON messages(author_did);
CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
CREATE INDEX IF NOT EXISTS idx_messages_reply_to_id ON messages(reply_to_id);

-- Message reactions
CREATE TABLE IF NOT EXISTS reactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL,
    user_did TEXT NOT NULL,                 -- User's ATProto DID
    emoji TEXT NOT NULL,                    -- Emoji reaction
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(message_id, user_did, emoji),
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_reactions_message_id ON reactions(message_id);

-- Attachments (references to object storage)
CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY,                    -- Attachment ID (UUID)
    message_id TEXT NOT NULL,               -- Message this attachment belongs to
    filename TEXT NOT NULL,                 -- Original filename
    content_type TEXT NOT NULL,             -- MIME type
    size_bytes INTEGER NOT NULL,            -- File size
    storage_key TEXT NOT NULL,              -- Key in object storage
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_attachments_message_id ON attachments(message_id);
"#;

    /// Migration to add message schema fields per message-schema.md spec
    pub const V0002_MESSAGE_SCHEMA_UPDATES: &str = r#"
-- Add thread_id for threading support
ALTER TABLE messages ADD COLUMN thread_id TEXT;

-- Add flags bitfield for message properties
ALTER TABLE messages ADD COLUMN flags INTEGER DEFAULT 0;

-- Add edited_at timestamp (replacing is_edited boolean)
ALTER TABLE messages ADD COLUMN edited_at TEXT;

-- Add expires_at for message TTL support
ALTER TABLE messages ADD COLUMN expires_at TEXT;

-- Make content nullable (for system messages with only embeds)
-- Note: SQLite doesn't support ALTER COLUMN, content is already TEXT which allows NULL

-- Add index for thread queries
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id, created_at);

-- Add index for channel + created_at (the most common query pattern)
CREATE INDEX IF NOT EXISTS idx_messages_channel_created ON messages(channel_id, created_at DESC);

-- Add index for expires_at to support TTL cleanup
CREATE INDEX IF NOT EXISTS idx_messages_expires ON messages(expires_at) WHERE expires_at IS NOT NULL;
"#;

    /// Get all per-waddle migrations in order
    pub fn all() -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                description: "Initial per-waddle schema".to_string(),
                sql: V0001_INITIAL_SCHEMA,
            },
            Migration {
                version: 2,
                description: "Add message schema fields per spec".to_string(),
                sql: V0002_MESSAGE_SCHEMA_UPDATES,
            },
        ]
    }
}

/// Migration runner for applying migrations to a database
pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    /// Create a new migration runner with the given migrations
    pub fn new(migrations: Vec<Migration>) -> Self {
        let mut sorted = migrations;
        sorted.sort_by_key(|m| m.version);
        Self { migrations: sorted }
    }

    /// Create a runner for global database migrations
    pub fn global() -> Self {
        Self::new(global::all())
    }

    /// Create a runner for per-waddle database migrations
    pub fn waddle() -> Self {
        Self::new(waddle::all())
    }

    /// Run all pending migrations on the database
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn run(&self, db: &Database) -> Result<Vec<i64>, DatabaseError> {
        // Use persistent connection for in-memory databases to ensure data persists
        // We need to handle both cases: in-memory (with persistent conn) and file-based
        if let Some(persistent) = db.persistent_connection() {
            // For in-memory databases, use the persistent connection
            let conn = persistent.lock().await;
            self.run_with_connection(&conn).await
        } else {
            // For file-based databases, create a new connection
            let conn = db.connect()?;
            self.run_with_connection(&conn).await
        }
    }

    /// Internal method to run migrations with a given connection
    async fn run_with_connection(&self, conn: &libsql::Connection) -> Result<Vec<i64>, DatabaseError> {

        // Ensure migrations table exists
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
            (),
        )
        .await
        .map_err(|e| DatabaseError::MigrationFailed(format!("Failed to create migrations table: {}", e)))?;

        // Get applied migrations
        let mut applied: Vec<i64> = Vec::new();
        let mut rows = conn
            .query("SELECT version FROM _migrations ORDER BY version", ())
            .await
            .map_err(|e| DatabaseError::MigrationFailed(format!("Failed to query migrations: {}", e)))?;

        while let Some(row) = rows.next().await.map_err(|e| {
            DatabaseError::MigrationFailed(format!("Failed to read migration row: {}", e))
        })? {
            let version: i64 = row.get(0).map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to get version from row: {}", e))
            })?;
            applied.push(version);
        }

        debug!("Already applied migrations: {:?}", applied);

        // Apply pending migrations
        let mut newly_applied = Vec::new();
        for migration in &self.migrations {
            if applied.contains(&migration.version) {
                debug!("Skipping already applied migration v{}", migration.version);
                continue;
            }

            info!(
                "Applying migration v{}: {}",
                migration.version, migration.description
            );

            // Execute migration SQL using batch execution
            conn.execute_batch(migration.sql)
                .await
                .map_err(|e| {
                    DatabaseError::MigrationFailed(format!(
                        "Migration v{} failed: {}",
                        migration.version, e
                    ))
                })?;

            // Record the migration
            conn.execute(
                "INSERT INTO _migrations (version, description) VALUES (?, ?)",
                (migration.version, migration.description.as_str()),
            )
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!(
                    "Failed to record migration v{}: {}",
                    migration.version, e
                ))
            })?;

            newly_applied.push(migration.version);
            info!("Applied migration v{}", migration.version);
        }

        if newly_applied.is_empty() {
            debug!("No new migrations to apply");
        } else {
            info!("Applied {} new migrations", newly_applied.len());
        }

        Ok(newly_applied)
    }

    /// Get the current schema version
    #[allow(dead_code)]
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn current_version(&self, db: &Database) -> Result<Option<i64>, DatabaseError> {
        // Use persistent connection for in-memory databases to ensure we see the same data
        if let Some(persistent) = db.persistent_connection() {
            let conn = persistent.lock().await;
            self.current_version_with_connection(&conn).await
        } else {
            let conn = db.connect()?;
            self.current_version_with_connection(&conn).await
        }
    }

    /// Internal method to get current version with a given connection
    #[allow(dead_code)]
    async fn current_version_with_connection(&self, conn: &libsql::Connection) -> Result<Option<i64>, DatabaseError> {
        // Check if migrations table exists
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='_migrations'",
                (),
            )
            .await
            .map_err(|e| DatabaseError::QueryFailed(format!("Failed to check migrations table: {}", e)))?;

        if rows.next().await.map_err(|e| {
            DatabaseError::QueryFailed(format!("Failed to read result: {}", e))
        })?.is_none()
        {
            return Ok(None);
        }

        // Get the latest version
        let mut rows = conn
            .query("SELECT MAX(version) FROM _migrations", ())
            .await
            .map_err(|e| DatabaseError::QueryFailed(format!("Failed to query max version: {}", e)))?;

        match rows.next().await.map_err(|e| {
            DatabaseError::QueryFailed(format!("Failed to read max version: {}", e))
        })? {
            Some(row) => {
                let version: Option<i64> = row.get(0).ok();
                Ok(version)
            }
            None => Ok(None),
        }
    }

    /// Check if there are pending migrations
    #[allow(dead_code)]
    pub async fn has_pending(&self, db: &Database) -> Result<bool, DatabaseError> {
        let current = self.current_version(db).await?.unwrap_or(0);
        let latest = self.migrations.last().map(|m| m.version).unwrap_or(0);
        Ok(current < latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_migration_runner_global() {
        let db = Database::in_memory("test-global").await.unwrap();
        let runner = MigrationRunner::global();

        // Run migrations
        let applied = runner.run(&db).await.unwrap();
        assert!(!applied.is_empty());

        // Running again should apply nothing
        let applied_again = runner.run(&db).await.unwrap();
        assert!(applied_again.is_empty());

        // Check version (4 migrations: initial schema + token endpoint + permission tuples + native users)
        let version = runner.current_version(&db).await.unwrap();
        assert_eq!(version, Some(4));
    }

    #[tokio::test]
    async fn test_migration_runner_waddle() {
        let db = Database::in_memory("test-waddle").await.unwrap();
        let runner = MigrationRunner::waddle();

        // Run migrations
        let applied = runner.run(&db).await.unwrap();
        assert!(!applied.is_empty());

        // Verify tables exist - use persistent connection for in-memory database
        let conn = db.persistent_connection().unwrap();
        let conn = conn.lock().await;
        let mut rows = conn
            .query("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name", ())
            .await
            .unwrap();

        let mut tables = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let name: String = row.get(0).unwrap();
            tables.push(name);
        }

        assert!(tables.contains(&"channels".to_string()));
        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"reactions".to_string()));
        assert!(tables.contains(&"attachments".to_string()));
    }

    #[tokio::test]
    async fn test_has_pending_migrations() {
        let db = Database::in_memory("test-pending").await.unwrap();
        let runner = MigrationRunner::global();

        // Should have pending migrations on fresh DB
        assert!(runner.has_pending(&db).await.unwrap());

        // Run migrations
        runner.run(&db).await.unwrap();

        // Should not have pending migrations
        assert!(!runner.has_pending(&db).await.unwrap());
    }
}
