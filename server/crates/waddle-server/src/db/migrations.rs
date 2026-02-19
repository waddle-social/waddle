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
use std::collections::HashMap;
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

/// Global database migrations (auth broker, users, permissions, and XMPP data)
pub mod global {
    use super::Migration;

    /// Hard-cut schema reset for native OIDC/OAuth auth broker.
    pub const V0001_AUTH_BROKER_SCHEMA: &str = r#"
PRAGMA foreign_keys = OFF;

DROP TABLE IF EXISTS auth_identities;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS waddle_members;
DROP TABLE IF EXISTS waddles;
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
    issuer TEXT,
    subject TEXT NOT NULL,
    email TEXT,
    email_verified INTEGER,
    raw_claims_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_login_at TEXT NOT NULL,
    UNIQUE(provider_id, subject),
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

CREATE TABLE waddles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    owner_id TEXT NOT NULL,
    icon_url TEXT,
    is_public INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (owner_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_waddles_owner_id ON waddles(owner_id);
CREATE INDEX idx_waddles_is_public ON waddles(is_public);

CREATE TABLE waddle_members (
    waddle_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'member',
    joined_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (waddle_id, user_id),
    FOREIGN KEY (waddle_id) REFERENCES waddles(id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

CREATE INDEX idx_waddle_members_user_id ON waddle_members(user_id);

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

    /// Get all global migrations in order
    pub fn all() -> Vec<Migration> {
        vec![Migration {
            version: 1,
            description: "Hard-cut auth broker and UUID principal schema".to_string(),
            sql: V0001_AUTH_BROKER_SCHEMA,
        }]
    }
}

/// Per-Waddle database migrations (channels, messages)
pub mod waddle {
    use super::Migration;

    /// Hard-cut per-waddle schema with UUID user principals.
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
    author_user_id TEXT NOT NULL,
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
CREATE INDEX idx_messages_author_user_id ON messages(author_user_id);
CREATE INDEX idx_messages_created_at ON messages(created_at);
CREATE INDEX idx_messages_reply_to_id ON messages(reply_to_id);
CREATE INDEX idx_messages_thread ON messages(thread_id, created_at);
CREATE INDEX idx_messages_channel_created ON messages(channel_id, created_at DESC);
CREATE INDEX idx_messages_expires ON messages(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE reactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(message_id, user_id, emoji),
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

    /// Get all per-waddle migrations in order
    pub fn all() -> Vec<Migration> {
        vec![Migration {
            version: 1,
            description: "Hard-cut per-waddle schema with user_id principals".to_string(),
            sql: V0001_SCHEMA,
        }]
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
    async fn run_with_connection(
        &self,
        conn: &libsql::Connection,
    ) -> Result<Vec<i64>, DatabaseError> {
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
        .map_err(|e| {
            DatabaseError::MigrationFailed(format!("Failed to create migrations table: {}", e))
        })?;

        // Get applied migrations (version + description).
        let mut applied_rows: Vec<(i64, String)> = Vec::new();
        let mut rows = conn
            .query(
                "SELECT version, description FROM _migrations ORDER BY version",
                (),
            )
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to query migrations: {}", e))
            })?;

        while let Some(row) = rows.next().await.map_err(|e| {
            DatabaseError::MigrationFailed(format!("Failed to read migration row: {}", e))
        })? {
            let version: i64 = row.get(0).map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to get version from row: {}", e))
            })?;
            let description: String = row.get(1).map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to get description from row: {}", e))
            })?;
            applied_rows.push((version, description));
        }

        // Hard-cut protection: if the migration history doesn't match this binary's
        // migration set (unknown versions or differing descriptions), reset migration
        // tracking and re-apply current migrations from scratch.
        let expected: HashMap<i64, &str> = self
            .migrations
            .iter()
            .map(|m| (m.version, m.description.as_str()))
            .collect();
        let has_incompatible_history = applied_rows.iter().any(|(version, description)| {
            expected
                .get(version)
                .map(|expected_desc| *expected_desc != description.as_str())
                .unwrap_or(true)
        });

        let applied: Vec<i64> = if has_incompatible_history {
            info!("Incompatible migration history detected, resetting migration tracking");
            conn.execute_batch("DROP TABLE IF EXISTS _migrations;")
                .await
                .map_err(|e| {
                    DatabaseError::MigrationFailed(format!(
                        "Failed to reset migration tracking table: {}",
                        e
                    ))
                })?;
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
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!(
                    "Failed to recreate migrations table: {}",
                    e
                ))
            })?;
            Vec::new()
        } else {
            applied_rows.iter().map(|(version, _)| *version).collect()
        };

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
            conn.execute_batch(migration.sql).await.map_err(|e| {
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
    async fn current_version_with_connection(
        &self,
        conn: &libsql::Connection,
    ) -> Result<Option<i64>, DatabaseError> {
        // Check if migrations table exists
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='_migrations'",
                (),
            )
            .await
            .map_err(|e| {
                DatabaseError::QueryFailed(format!("Failed to check migrations table: {}", e))
            })?;

        if rows
            .next()
            .await
            .map_err(|e| DatabaseError::QueryFailed(format!("Failed to read result: {}", e)))?
            .is_none()
        {
            return Ok(None);
        }

        // Get the latest version
        let mut rows = conn
            .query("SELECT MAX(version) FROM _migrations", ())
            .await
            .map_err(|e| {
                DatabaseError::QueryFailed(format!("Failed to query max version: {}", e))
            })?;

        match rows
            .next()
            .await
            .map_err(|e| DatabaseError::QueryFailed(format!("Failed to read max version: {}", e)))?
        {
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

        // Check version (single hard-cut schema migration)
        let version = runner.current_version(&db).await.unwrap();
        assert_eq!(version, Some(1));
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
            .query(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
                (),
            )
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

    #[tokio::test]
    async fn test_incompatible_history_forces_hard_cut_reapply() {
        let db = Database::in_memory("test-incompatible-history")
            .await
            .unwrap();
        let conn = db.persistent_connection().unwrap();
        let conn = conn.lock().await;

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
        .unwrap();
        conn.execute(
            "INSERT INTO _migrations (version, description) VALUES (1, 'legacy initial schema')",
            (),
        )
        .await
        .unwrap();
        drop(conn);

        let runner = MigrationRunner::global();
        let applied = runner.run(&db).await.unwrap();
        assert_eq!(applied, vec![1]);

        let applied_again = runner.run(&db).await.unwrap();
        assert!(applied_again.is_empty());

        let version = runner.current_version(&db).await.unwrap();
        assert_eq!(version, Some(1));
    }
}
