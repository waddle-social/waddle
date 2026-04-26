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

use super::{Database, DatabaseDriver, DatabaseError};
use std::collections::HashMap;
use tracing::{debug, info, instrument};

/// Represents a single database migration with driver-specific SQL.
///
/// Each migration carries separate SQL for SQLite and Postgres so the
/// runner can apply the correct dialect without any runtime rewriting.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Version number (must be unique and incrementing)
    pub version: i64,
    /// Description of what this migration does
    pub description: String,
    /// SQL to execute on SQLite
    pub sql_sqlite: &'static str,
    /// SQL to execute on Postgres
    pub sql_postgres: &'static str,
}

impl Migration {
    /// Return the SQL appropriate for the given driver.
    pub fn sql_for(&self, driver: DatabaseDriver) -> &'static str {
        match driver {
            DatabaseDriver::Sqlite => self.sql_sqlite,
            DatabaseDriver::Postgres => self.sql_postgres,
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

    /// Get all global migrations in order
    pub fn all() -> Vec<Migration> {
        vec![Migration {
            version: 1,
            description: "Hard-cut auth broker schema with roster pre-approval".to_string(),
            sql_sqlite: V0001_AUTH_BROKER_SCHEMA,
            sql_postgres: V0001_AUTH_BROKER_SCHEMA_POSTGRES,
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

    /// Hard-cut per-waddle schema with UUID user principals — Postgres dialect.
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
    author_user_id TEXT NOT NULL,
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
CREATE INDEX idx_messages_author_user_id ON messages(author_user_id);
CREATE INDEX idx_messages_created_at ON messages(created_at);
CREATE INDEX idx_messages_reply_to_id ON messages(reply_to_id);
CREATE INDEX idx_messages_thread ON messages(thread_id, created_at);
CREATE INDEX idx_messages_channel_created ON messages(channel_id, created_at DESC);
CREATE INDEX idx_messages_expires ON messages(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE reactions (
    id BIGSERIAL PRIMARY KEY,
    message_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    emoji TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
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
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachments_message_id ON attachments(message_id);
"#;

    /// Get all waddle schema migrations in order.
    ///
    /// Versions are intentionally offset from global migrations so a single
    /// database can safely apply both sets without migration history collisions.
    pub fn all() -> Vec<Migration> {
        vec![Migration {
            version: 1001,
            description: "Hard-cut per-waddle schema with user_id principals".to_string(),
            sql_sqlite: V0001_SCHEMA,
            sql_postgres: V0001_SCHEMA_POSTGRES,
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
    #[cfg(test)]
    pub fn global() -> Self {
        Self::single()
    }

    /// Create a runner for channel/message schema migrations.
    #[cfg(test)]
    pub fn waddle() -> Self {
        Self::new(waddle::all())
    }

    /// Create a runner for single-database mode.
    ///
    /// This composes global + channel/message schema migrations into one ordered
    /// stream so they share one migration history without version collisions.
    pub fn single() -> Self {
        let mut migrations = global::all();
        migrations.extend(waddle::all());
        Self::new(migrations)
    }

    /// Run all pending migrations on the database
    #[instrument(skip_all, fields(db_name = %db.name()))]
    pub async fn run(&self, db: &Database) -> Result<Vec<i64>, DatabaseError> {
        let conn = db.guard().await?;
        self.run_with_connection(&conn, db.driver()).await
    }

    /// Internal method to run migrations with a given connection
    async fn run_with_connection(
        &self,
        conn: &super::ConnectionGuard,
        driver: DatabaseDriver,
    ) -> Result<Vec<i64>, DatabaseError> {
        // Ensure migrations table exists
        conn.execute(migrations_table_sql(driver), ())
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
            conn.execute(migrations_table_sql(driver), ())
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

            // Execute migration SQL using batch execution (driver-specific dialect)
            let sql = migration.sql_for(driver);
            conn.execute_batch(sql).await.map_err(|e| {
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
        let conn = db.guard().await?;
        self.current_version_with_connection(&conn, db.driver())
            .await
    }

    /// Internal method to get current version with a given connection
    #[allow(dead_code)]
    async fn current_version_with_connection(
        &self,
        conn: &super::ConnectionGuard,
        driver: DatabaseDriver,
    ) -> Result<Option<i64>, DatabaseError> {
        conn.execute(migrations_table_sql(driver), ())
            .await
            .map_err(|e| {
                DatabaseError::MigrationFailed(format!("Failed to ensure migrations table: {}", e))
            })?;

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

fn migrations_table_sql(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => {
            r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#
        }
        DatabaseDriver::Postgres => {
            r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#
        }
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

        // Check version (global + shared waddle schema)
        let version = runner.current_version(&db).await.unwrap();
        assert_eq!(version, Some(1001));
    }

    #[tokio::test]
    async fn test_migration_runner_waddle() {
        let db = Database::in_memory("test-waddle").await.unwrap();
        let runner = MigrationRunner::waddle();

        // Run migrations
        let applied = runner.run(&db).await.unwrap();
        assert!(!applied.is_empty());

        // Verify tables exist
        let conn = db.guard().await.unwrap();
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
        let conn = db.guard().await.unwrap();

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
        assert_eq!(applied, vec![1, 1001]);

        let applied_again = runner.run(&db).await.unwrap();
        assert!(applied_again.is_empty());

        let version = runner.current_version(&db).await.unwrap();
        assert_eq!(version, Some(1001));
    }

    #[tokio::test]
    async fn test_incompatible_history_recreates_existing_owned_tables() {
        let db = Database::in_memory("test-incompatible-existing-tables")
            .await
            .unwrap();
        let conn = db.guard().await.unwrap();

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
        conn.execute(
            r#"
            CREATE TABLE roster_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_jid TEXT NOT NULL,
                contact_jid TEXT NOT NULL,
                subscription TEXT NOT NULL DEFAULT 'none'
            )
            "#,
            (),
        )
        .await
        .unwrap();
        drop(conn);

        let runner = MigrationRunner::global();
        let applied = runner.run(&db).await.unwrap();
        assert_eq!(applied, vec![1, 1001]);

        let conn = db.guard().await.unwrap();
        let mut rows = conn
            .query(
                r#"
                SELECT COUNT(*)
                FROM pragma_table_info('roster_items')
                WHERE name = 'approved'
                "#,
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let has_approved: i64 = row.get(0).unwrap();
        assert_eq!(has_approved, 1);
    }

    // --- Postgres dialect validation (no live DB required) ---
    //
    // These tests verify that every Postgres-dialect migration SQL:
    //   - is non-empty
    //   - contains no SQLite-only syntax (PRAGMA, AUTOINCREMENT, datetime('now'), bare BLOB type)
    //   - uses DROP ... CASCADE instead of bare DROP TABLE
    // and that SQLite SQL:
    //   - contains no Postgres-only syntax (BIGSERIAL, BYTEA, CASCADE drops)

    fn sqlite_only_patterns() -> Vec<&'static str> {
        vec!["PRAGMA ", "AUTOINCREMENT", "datetime('now')", " BLOB "]
    }

    fn postgres_only_patterns() -> Vec<&'static str> {
        vec!["BIGSERIAL", "BYTEA", "::TEXT"]
    }

    #[test]
    fn postgres_global_v0001_has_no_sqlite_syntax() {
        let sql = global::V0001_AUTH_BROKER_SCHEMA_POSTGRES;
        assert!(
            !sql.is_empty(),
            "Postgres global V0001 SQL must not be empty"
        );
        for pat in sqlite_only_patterns() {
            assert!(
                !sql.contains(pat),
                "Postgres global V0001 SQL must not contain SQLite-only pattern: {pat}"
            );
        }
        assert!(
            sql.contains("CASCADE"),
            "Postgres global V0001 DROP TABLE statements must use CASCADE"
        );
    }

    #[test]
    fn postgres_waddle_v0001_has_no_sqlite_syntax() {
        let sql = waddle::V0001_SCHEMA_POSTGRES;
        assert!(
            !sql.is_empty(),
            "Postgres waddle V0001 SQL must not be empty"
        );
        for pat in sqlite_only_patterns() {
            assert!(
                !sql.contains(pat),
                "Postgres waddle V0001 SQL must not contain SQLite-only pattern: {pat}"
            );
        }
        assert!(
            sql.contains("CASCADE"),
            "Postgres waddle V0001 DROP TABLE statements must use CASCADE"
        );
    }

    #[test]
    fn sqlite_global_v0001_has_no_postgres_syntax() {
        let sql = global::V0001_AUTH_BROKER_SCHEMA;
        for pat in postgres_only_patterns() {
            assert!(
                !sql.contains(pat),
                "SQLite global V0001 SQL must not contain Postgres-only pattern: {pat}"
            );
        }
    }

    #[test]
    fn sqlite_waddle_v0001_has_no_postgres_syntax() {
        let sql = waddle::V0001_SCHEMA;
        for pat in postgres_only_patterns() {
            assert!(
                !sql.contains(pat),
                "SQLite waddle V0001 SQL must not contain Postgres-only pattern: {pat}"
            );
        }
    }

    #[test]
    fn migration_sql_for_returns_correct_dialect() {
        let m = Migration {
            version: 1,
            description: "test".to_string(),
            sql_sqlite: "SELECT 1",
            sql_postgres: "SELECT 2",
        };
        assert_eq!(m.sql_for(DatabaseDriver::Sqlite), "SELECT 1");
        assert_eq!(m.sql_for(DatabaseDriver::Postgres), "SELECT 2");
    }

    #[test]
    fn all_migrations_have_non_empty_postgres_sql() {
        for m in MigrationRunner::single().migrations {
            assert!(
                !m.sql_postgres.is_empty(),
                "Migration v{} has empty Postgres SQL",
                m.version
            );
            assert!(
                !m.sql_sqlite.is_empty(),
                "Migration v{} has empty SQLite SQL",
                m.version
            );
        }
    }
}
