//! Read-only validation of an owner-prepared pending-delivery database.

use std::collections::HashMap;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

use super::{recovery_order, schema};

#[derive(Debug, thiserror::Error)]
pub enum PendingRuntimeAttachError {
    #[error("PostgreSQL pending-delivery attachment requires a physically attested pool")]
    AttestedPostgresPoolRequired,
    #[error("pending-delivery attachment requires the main database admission lane")]
    MainLaneRequired,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("SQLite has a temporary pending-delivery shadow or trigger")]
    SqliteTemporaryObject,
    #[error("an in-memory SQLite pending-delivery pool must have exactly one connection")]
    SqliteMemoryPoolMustBeSingleConnection,
    #[error("could not verify the PostgreSQL {setting} bound for pending-delivery attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect pending-delivery relation {relation}: {source}")]
    Relation {
        relation: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("pending-delivery relation {table} is not an ordinary table")]
    InvalidTable { table: &'static str },
    #[error("pending-delivery relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("pending-delivery relation {table} has an invalid primary key")]
    InvalidPrimaryKey { table: &'static str },
    #[error("pending-delivery index {index} is missing or has an invalid definition")]
    InvalidIndex { index: &'static str },
    #[error("pending-delivery owner migration marker {marker} is missing")]
    MissingMarker { marker: &'static str },
    #[error("pending-delivery recovery metadata is missing or invalid: {component}")]
    InvalidRecovery { component: &'static str },
    #[error("SQLite pending-delivery verification task failed: {0}")]
    VerificationTask(#[source] tokio::task::JoinError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// The owned SQLite task finishes even if its caller is cancelled. In
/// particular, an interrupted SQLx pool acquisition/transaction must not
/// discard the only physical connection holding a private memory database.
pub(super) async fn verify(db: &Database) -> Result<(), PendingRuntimeAttachError> {
    if db.driver() == DatabaseDriver::Sqlite {
        let db = db.clone();
        tokio::spawn(async move { verify_snapshot(&db).await })
            .await
            .map_err(PendingRuntimeAttachError::VerificationTask)?
    } else {
        verify_snapshot(db).await
    }
}

async fn verify_snapshot(db: &Database) -> Result<(), PendingRuntimeAttachError> {
    if db.driver() == DatabaseDriver::Postgres && !db.is_main_lane() {
        return Err(PendingRuntimeAttachError::MainLaneRequired);
    }
    let capability = if db.driver() == DatabaseDriver::Postgres {
        let capability = db
            .attested_pg_writer_capability()
            .ok_or(PendingRuntimeAttachError::AttestedPostgresPoolRequired)?;
        db.verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;
        Some(capability)
    } else {
        None
    };
    let mut tx = db.begin().await?;
    // A pooled external SQLite connection may have a longer busy timeout than
    // the native pool. Tighten its lock wait during inspection and restore it
    // on every normal/error exit before returning the connection to its pool.
    let sqlite_timeout = if db.driver() == DatabaseDriver::Sqlite {
        let mut rows = tx.query("PRAGMA busy_timeout", ()).await?;
        let original: i64 = rows
            .next()
            .await?
            .ok_or_else(|| {
                DatabaseError::QueryFailed("SQLite busy_timeout returned no row".to_owned())
            })?
            .get(0)?;
        if original > 5_000 {
            tx.execute("PRAGMA busy_timeout = 5000", ()).await?;
            Some(original)
        } else {
            None
        }
    } else {
        None
    };
    let result = verify_transaction(db, &mut tx, capability).await;
    if let Some(original) = sqlite_timeout {
        tx.execute(&format!("PRAGMA busy_timeout = {original}"), ())
            .await?;
    }
    result?;
    tx.commit().await?;
    Ok(())
}

async fn verify_transaction(
    db: &Database,
    tx: &mut Transaction<'_>,
    capability: Option<crate::db::physical_attestation::AttestedPostgresWriterCapability>,
) -> Result<(), PendingRuntimeAttachError> {
    let schema_name = if let Some(capability) = capability {
        // Refuse an inherited snapshot, then open a capability-bound snapshot.
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (),
        )
        .await?;
        crate::db::read_only_timeout::bound_postgres_waits(tx)
            .await
            .map_err(|error| match error {
                crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                    PendingRuntimeAttachError::InvalidTimeoutSetting { setting }
                }
                crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                    PendingRuntimeAttachError::Database(error)
                }
            })?;
        let expected = &capability.identity().schema.name.0;
        let quoted = format!("\"{}\"", expected.replace('"', "\"\""));
        // The pool checks the physical connection on checkout. Lock each
        // qualified table before the first repeatable-read snapshot: an owner
        // could otherwise commit table DDL after attestation pins the snapshot
        // but before the later SELECT takes its relation lock.
        for table in [
            "pending_delivery",
            "pending_delivery_sm_promotions",
            "pending_delivery_startup_migrations",
        ] {
            tx.execute(
                &format!("LOCK TABLE {quoted}.\"{table}\" IN ACCESS SHARE MODE"),
                (),
            )
            .await
            .map_err(|source| PendingRuntimeAttachError::Relation {
                relation: table,
                source,
            })?;
        }
        capability.attest_transaction(tx).await?;
        let mut rows = tx.query("SELECT pg_catalog.current_schema()", ()).await?;
        let active: Option<String> = rows
            .next()
            .await?
            .ok_or(PendingRuntimeAttachError::ActiveSchemaMismatch)?
            .get(0)?;
        if active.as_deref() != Some(expected) {
            return Err(PendingRuntimeAttachError::ActiveSchemaMismatch);
        }
        expected.clone()
    } else {
        let mut databases = tx.query("PRAGMA database_list", ()).await?;
        let mut main_in_memory = None;
        while let Some(row) = databases.next().await? {
            if row.get::<String>(1)? == "main" {
                let file: String = row.get(2)?;
                main_in_memory = Some(file.is_empty() || file == ":memory:");
            }
        }
        if main_in_memory != Some(false)
            && db
                .sqlite_pool()
                .is_some_and(|pool| pool.options().get_max_connections() != 1)
        {
            return Err(PendingRuntimeAttachError::SqliteMemoryPoolMustBeSingleConnection);
        }
        if main_in_memory.is_none() {
            return Err(
                DatabaseError::QueryFailed("SQLite main database is missing".to_owned()).into(),
            );
        }
        let mut rows = tx
            .query(
                "SELECT 1 FROM sqlite_temp_master WHERE \
             (type IN ('table', 'view') AND name COLLATE NOCASE IN \
             ('pending_delivery', 'pending_delivery_sm_promotions', \
              'pending_delivery_startup_migrations', 'pending_notification_recovery_counter', \
              'sm_ingress_appends', 'sm_sessions', 'sm_unacked', 'mam_messages', 'clustering_claims')) \
             OR (type = 'trigger' AND tbl_name COLLATE NOCASE IN \
             ('pending_delivery', 'pending_delivery_sm_promotions', \
              'pending_delivery_startup_migrations', 'pending_notification_recovery_counter', \
              'sm_ingress_appends', 'sm_sessions', 'sm_unacked', 'mam_messages', 'clustering_claims')) LIMIT 1",
                (),
            )
            .await?;
        if rows.next().await?.is_some() {
            return Err(PendingRuntimeAttachError::SqliteTemporaryObject);
        }
        "main".to_owned()
    };
    let quoted = format!("\"{}\"", schema_name.replace('"', "\"\""));
    for (table, columns) in [
        ("pending_delivery", "row_id, recipient_jid, original_receipt_at, payload_kind, archive_stanza_by, archive_stanza_id, transient_xml, flushed_in_session, outbound_sequence, notification_outboxed_at_ms, claimed_at_ms, claim_token, claim_offered, origin_stream_id, inbound_seq, pair_sequence, notification_recovery_ordinal"),
        ("pending_delivery_sm_promotions", "origin_stream_id, sequence, created_at_ms"),
        ("pending_delivery_startup_migrations", "name, completed_at"),
    ] {
        tx.query(&format!("SELECT {columns} FROM {quoted}.\"{table}\" WHERE 1 = 0"), ())
            .await
            .map_err(|source| PendingRuntimeAttachError::Relation { relation: table, source })?;
        verify_table(tx, db.driver(), &schema_name, table).await?;
    }
    if db.driver() == DatabaseDriver::Sqlite {
        tx.query("SELECT singleton, last_ordinal FROM main.pending_notification_recovery_counter WHERE 1 = 0", ())
            .await.map_err(|source| PendingRuntimeAttachError::Relation {
                relation: "pending_notification_recovery_counter", source,
            })?;
        verify_table(
            tx,
            db.driver(),
            &schema_name,
            "pending_notification_recovery_counter",
        )
        .await?;
    }
    verify_columns_and_keys(tx, db.driver(), &schema_name).await?;
    verify_indexes(tx, db.driver(), &schema_name).await?;
    for marker in [
        schema::V1014_CLAIM_RESET_MARKER,
        schema::LEGACY_MAM_CLEANUP_MARKER,
    ] {
        let sqlite_type_check = if db.driver() == DatabaseDriver::Sqlite {
            " AND typeof(completed_at) = 'integer'"
        } else {
            ""
        };
        let mut rows = tx.query(
            &format!("SELECT 1 FROM {quoted}.pending_delivery_startup_migrations WHERE name = ?{sqlite_type_check} LIMIT 1"),
            crate::db_params![marker],
        ).await?;
        if rows.next().await?.is_none() {
            return Err(PendingRuntimeAttachError::MissingMarker { marker });
        }
    }
    match db.driver() {
        DatabaseDriver::Postgres => verify_postgres_recovery(tx, &schema_name).await?,
        DatabaseDriver::Sqlite => verify_sqlite_recovery(tx).await?,
    }
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    schema: &str,
    table: &'static str,
) -> Result<(), PendingRuntimeAttachError> {
    let mut rows = match driver {
        DatabaseDriver::Postgres => tx.query(
            "SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r'",
            crate::db_params![schema, table],
        ).await?,
        DatabaseDriver::Sqlite => tx.query(
            "SELECT 1 FROM main.sqlite_master WHERE type = 'table' AND name = ?",
            crate::db_params![table],
        ).await?,
    };
    if rows.next().await?.is_none() {
        return Err(PendingRuntimeAttachError::InvalidTable { table });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ColumnType {
    Text,
    Integer,
    Bigint,
}
struct ColumnSpec {
    name: &'static str,
    kind: ColumnType,
    required: bool,
}
const fn col(name: &'static str, kind: ColumnType, required: bool) -> ColumnSpec {
    ColumnSpec {
        name,
        kind,
        required,
    }
}

const PENDING_COLUMNS: &[ColumnSpec] = &[
    col("row_id", ColumnType::Text, false),
    col("recipient_jid", ColumnType::Text, true),
    col("original_receipt_at", ColumnType::Bigint, true),
    col("payload_kind", ColumnType::Text, true),
    col("archive_stanza_by", ColumnType::Text, false),
    col("archive_stanza_id", ColumnType::Text, false),
    col("transient_xml", ColumnType::Text, false),
    col("flushed_in_session", ColumnType::Text, false),
    col("outbound_sequence", ColumnType::Integer, false),
    col("notification_outboxed_at_ms", ColumnType::Bigint, false),
    col("claimed_at_ms", ColumnType::Bigint, false),
    col("claim_token", ColumnType::Text, false),
    col("claim_offered", ColumnType::Integer, true),
    col("origin_stream_id", ColumnType::Text, false),
    col("inbound_seq", ColumnType::Bigint, false),
    col("pair_sequence", ColumnType::Bigint, false),
    col("notification_recovery_ordinal", ColumnType::Bigint, false),
];
const PROMOTION_COLUMNS: &[ColumnSpec] = &[
    col("origin_stream_id", ColumnType::Text, true),
    col("sequence", ColumnType::Bigint, true),
    col("created_at_ms", ColumnType::Bigint, true),
];
const MARKER_COLUMNS: &[ColumnSpec] = &[
    col("name", ColumnType::Text, false),
    col("completed_at", ColumnType::Bigint, true),
];
const COUNTER_COLUMNS: &[ColumnSpec] = &[
    col("singleton", ColumnType::Integer, false),
    col("last_ordinal", ColumnType::Integer, true),
];

async fn verify_columns_and_keys(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    schema: &str,
) -> Result<(), PendingRuntimeAttachError> {
    for (table, columns, expected_key) in [
        ("pending_delivery", PENDING_COLUMNS, &["row_id"][..]),
        (
            "pending_delivery_sm_promotions",
            PROMOTION_COLUMNS,
            &["origin_stream_id", "sequence"][..],
        ),
        (
            "pending_delivery_startup_migrations",
            MARKER_COLUMNS,
            &["name"][..],
        ),
        (
            "pending_notification_recovery_counter",
            COUNTER_COLUMNS,
            &["singleton"][..],
        ),
    ] {
        if driver == DatabaseDriver::Postgres && table == "pending_notification_recovery_counter" {
            continue;
        }
        let mut actual = HashMap::new();
        let mut keys = Vec::new();
        match driver {
            DatabaseDriver::Postgres => {
                let mut rows = tx.query(
                    "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull \
                     FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class t ON t.oid = a.attrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     WHERE n.nspname = ? AND t.relname = ? AND a.attnum > 0 AND NOT a.attisdropped",
                    crate::db_params![schema, table],
                ).await?;
                while let Some(row) = rows.next().await? {
                    actual.insert(
                        row.get::<String>(0)?,
                        (row.get::<String>(1)?, row.get::<bool>(2)?),
                    );
                }
                let mut rows = tx.query(
                    "SELECT a.attname::text FROM pg_catalog.pg_constraint c \
                     JOIN pg_catalog.pg_class t ON t.oid = c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) WITH ORDINALITY k(attnum, position) \
                     JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                     WHERE c.contype = 'p' AND NOT c.condeferrable AND c.convalidated \
                       AND EXISTS (SELECT 1 FROM pg_catalog.pg_index pi WHERE pi.indexrelid = c.conindid \
                                   AND pi.indisvalid AND pi.indisready AND pi.indislive) \
                       AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
                    crate::db_params![schema, table],
                ).await?;
                while let Some(row) = rows.next().await? {
                    keys.push(row.get::<String>(0)?);
                }
            }
            DatabaseDriver::Sqlite => {
                // All table names originate in the fixed list above.
                let mut rows = tx
                    .query(&format!("PRAGMA main.table_info(\"{table}\")"), ())
                    .await?;
                let mut positions = Vec::new();
                while let Some(row) = rows.next().await? {
                    let name: String = row.get(1)?;
                    let position: i64 = row.get(5)?;
                    if position > 0 {
                        positions.push((position, name.clone()));
                    }
                    actual.insert(
                        name,
                        (
                            row.get::<String>(2)?.to_ascii_lowercase(),
                            row.get::<i64>(3)? != 0,
                        ),
                    );
                }
                positions.sort_by_key(|(position, _)| *position);
                keys.extend(positions.into_iter().map(|(_, name)| name));
            }
        }
        if keys.iter().map(String::as_str).collect::<Vec<_>>() != expected_key {
            return Err(PendingRuntimeAttachError::InvalidPrimaryKey { table });
        }
        for column in columns {
            let Some((kind, not_null)) = actual.get(column.name) else {
                return Err(PendingRuntimeAttachError::InvalidColumn {
                    table,
                    column: column.name,
                });
            };
            let valid_type = match (driver, column.kind) {
                (_, ColumnType::Text) => kind == "text",
                (DatabaseDriver::Postgres, ColumnType::Integer) => kind == "integer",
                (DatabaseDriver::Postgres, ColumnType::Bigint) => kind == "bigint",
                (DatabaseDriver::Sqlite, ColumnType::Integer) => kind == "integer",
                (DatabaseDriver::Sqlite, ColumnType::Bigint) => {
                    kind == "integer" || kind == "bigint"
                }
            };
            let requires_not_null = column.required
                || (driver == DatabaseDriver::Postgres
                    && (expected_key.contains(&column.name)
                        || column.name == "notification_recovery_ordinal"));
            if !valid_type || (requires_not_null && !not_null) {
                return Err(PendingRuntimeAttachError::InvalidColumn {
                    table,
                    column: column.name,
                });
            }
        }
    }
    Ok(())
}

struct IndexSpec {
    name: &'static str,
    unique: bool,
    sqlite: &'static str,
    postgres_suffix: &'static str,
}

const INDEXES: &[IndexSpec] = &[
    IndexSpec {
        name: "idx_pending_delivery_dedup", unique: true,
        sqlite: "CREATE UNIQUE INDEX idx_pending_delivery_dedup ON pending_delivery (recipient_jid, origin_stream_id, inbound_seq) WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL",
        postgres_suffix: "USING btree (recipient_jid, origin_stream_id, inbound_seq) WHERE ((origin_stream_id IS NOT NULL) AND (inbound_seq IS NOT NULL))",
    },
    IndexSpec {
        name: "idx_pending_delivery_recipient", unique: false,
        sqlite: "CREATE INDEX idx_pending_delivery_recipient ON pending_delivery (recipient_jid, row_id)",
        postgres_suffix: "USING btree (recipient_jid, row_id)",
    },
    IndexSpec {
        name: "idx_pending_delivery_session", unique: false,
        sqlite: "CREATE INDEX idx_pending_delivery_session ON pending_delivery (flushed_in_session)",
        postgres_suffix: "USING btree (flushed_in_session)",
    },
    IndexSpec {
        name: "idx_pending_delivery_archived_unique", unique: true,
        sqlite: "CREATE UNIQUE INDEX idx_pending_delivery_archived_unique ON pending_delivery (recipient_jid, archive_stanza_id) WHERE payload_kind = 'archived'",
        postgres_suffix: "USING btree (recipient_jid, archive_stanza_id) WHERE (payload_kind = 'archived'::text)",
    },
    IndexSpec {
        name: "idx_pending_delivery_notification_outbox", unique: false,
        sqlite: "CREATE INDEX idx_pending_delivery_notification_outbox ON pending_delivery (notification_outboxed_at_ms, row_id) WHERE payload_kind = 'archived' AND flushed_in_session IS NULL",
        postgres_suffix: "USING btree (notification_outboxed_at_ms, row_id) WHERE ((payload_kind = 'archived'::text) AND (flushed_in_session IS NULL))",
    },
    IndexSpec {
        name: "idx_pending_notification_recovery_ordinal", unique: true,
        sqlite: "CREATE UNIQUE INDEX idx_pending_notification_recovery_ordinal ON pending_delivery (notification_recovery_ordinal)",
        postgres_suffix: "USING btree (notification_recovery_ordinal)",
    },
    IndexSpec {
        name: "idx_pending_notification_recovery_due", unique: false,
        sqlite: "CREATE INDEX idx_pending_notification_recovery_due ON pending_delivery (notification_recovery_ordinal) WHERE payload_kind = 'archived' AND flushed_in_session IS NULL AND notification_outboxed_at_ms IS NULL",
        postgres_suffix: "USING btree (notification_recovery_ordinal) WHERE ((payload_kind = 'archived'::text) AND (flushed_in_session IS NULL) AND (notification_outboxed_at_ms IS NULL))",
    },
];

// Ignore formatting outside string literals, but preserve quoted bytes: a
// changed predicate or trigger message must not normalize to the original.
fn normalize_sql(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut quoted = false;
    let mut whitespace = false;
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if whitespace && !result.is_empty() {
                result.push(' ');
            }
            whitespace = false;
            result.push(ch);
            if quoted && chars.peek() == Some(&'\'') {
                result.push('\'');
                chars.next();
            } else {
                quoted = !quoted;
            }
        } else if quoted {
            result.push(ch);
        } else if ch.is_ascii_whitespace() {
            whitespace = true;
        } else {
            if whitespace && !result.is_empty() {
                result.push(' ');
            }
            whitespace = false;
            result.push(ch.to_ascii_lowercase());
        }
    }
    result
}

// sqlite_master has no parsed CHECK-constraint catalog. Inspect the actual
// top-level column declaration, so a copy of the check in a comment, string,
// or another column cannot stand in for the ordinal's own constraint.
fn sqlite_ordinal_declaration_is_valid(sql: &str) -> bool {
    const EXPECTED: &str = "notification_recovery_ordinal INTEGER CHECK (notification_recovery_ordinal IS NULL OR (typeof(notification_recovery_ordinal) = 'integer' AND notification_recovery_ordinal > 0))";
    let bytes = sql.as_bytes();
    let mut depth = 0_u32;
    let mut start = 0;
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\'' | b'"' | b'`' | b'[' => {
                let end = if bytes[pos] == b'[' { b']' } else { bytes[pos] };
                pos += 1;
                loop {
                    if pos == bytes.len() {
                        return false;
                    }
                    if bytes[pos] == end {
                        if end != b']' && bytes.get(pos + 1) == Some(&end) {
                            pos += 2;
                        } else {
                            break;
                        }
                    } else {
                        pos += 1;
                    }
                }
            }
            b'-' if bytes.get(pos + 1) == Some(&b'-') => {
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                continue;
            }
            b'/' if bytes.get(pos + 1) == Some(&b'*') => {
                pos += 2;
                while pos + 1 < bytes.len() && &bytes[pos..pos + 2] != b"*/" {
                    pos += 1;
                }
                if pos + 1 >= bytes.len() {
                    return false;
                }
                pos += 2;
                continue;
            }
            b'(' => {
                depth += 1;
                if depth == 1 {
                    start = pos + 1;
                }
            }
            b')' if depth == 0 => return false,
            b')' if depth == 1 => {
                return normalize_sql(&sql[start..pos]) == normalize_sql(EXPECTED);
            }
            b')' => depth -= 1,
            b',' if depth == 1 => {
                if normalize_sql(&sql[start..pos]) == normalize_sql(EXPECTED) {
                    return true;
                }
                start = pos + 1;
            }
            _ => {}
        }
        pos += 1;
    }
    false
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    schema: &str,
) -> Result<(), PendingRuntimeAttachError> {
    for index in INDEXES {
        let valid = match driver {
            DatabaseDriver::Postgres => {
                let mut rows = tx
                    .query(
                        "SELECT x.indisunique, pg_catalog.pg_get_indexdef(x.indexrelid) \
                     FROM pg_catalog.pg_index x JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid \
                     JOIN pg_catalog.pg_class t ON t.oid = x.indrelid \
                     JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                     WHERE n.nspname = ? AND t.relname = 'pending_delivery' AND i.relname = ? \
                       AND x.indisvalid AND x.indisready AND x.indislive",
                        crate::db_params![schema, index.name],
                    )
                    .await?;
                match rows.next().await? {
                    Some(row) => {
                        let unique: bool = row.get(0)?;
                        let definition: String = row.get(1)?;
                        unique == index.unique
                            && definition
                                .split_once(" USING ")
                                .map(|(_, suffix)| {
                                    normalize_sql(&format!("USING {suffix}"))
                                        == normalize_sql(index.postgres_suffix)
                                })
                                .unwrap_or(false)
                    }
                    None => false,
                }
            }
            DatabaseDriver::Sqlite => {
                let mut rows = tx.query(
                    "SELECT sql FROM main.sqlite_master WHERE type = 'index' AND tbl_name = 'pending_delivery' AND name = ?",
                    crate::db_params![index.name],
                ).await?;
                match rows.next().await? {
                    Some(row) => {
                        let definition: Option<String> = row.get(0)?;
                        definition.as_deref().map(normalize_sql)
                            == Some(normalize_sql(index.sqlite))
                    }
                    None => false,
                }
            }
        };
        if !valid {
            return Err(PendingRuntimeAttachError::InvalidIndex { index: index.name });
        }
    }
    Ok(())
}

async fn verify_postgres_recovery(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PendingRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT a.attidentity::text, s.seqtypid = 'pg_catalog.int8'::pg_catalog.regtype \
                  AND s.seqstart = 1 AND s.seqincrement = 1 AND s.seqmin = 1 \
                  AND s.seqmax = 9223372036854775807 AND s.seqcache = 1 AND NOT s.seqcycle \
         FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attname = 'notification_recovery_ordinal' AND NOT a.attisdropped \
         JOIN pg_catalog.pg_depend d ON d.refobjid = t.oid AND d.refobjsubid = a.attnum \
           AND d.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass AND d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass \
           AND d.objsubid = 0 AND d.deptype = 'i' \
         JOIN pg_catalog.pg_class seq ON seq.oid = d.objid AND seq.relkind = 'S' AND seq.relnamespace = n.oid \
         JOIN pg_catalog.pg_sequence s ON s.seqrelid = seq.oid \
         WHERE n.nspname = ? AND t.relname = 'pending_delivery'",
        crate::db_params![schema],
    ).await?;
    let valid = match rows.next().await? {
        Some(row) => {
            row.get::<String>(0)? == "a" && row.get::<bool>(1)? && rows.next().await?.is_none()
        }
        None => false,
    };
    if !valid {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL identity sequence",
        });
    }
    drop(rows);
    let mut rows = tx.query(
        "SELECT p.prosrc, t.tgenabled::text IN ('O', 'A') AND t.tgtype = 19 \
                AND t.tgattr::text = a.attnum::text AND t.tgqual IS NULL AND NOT t.tgisinternal \
                AND t.tgconstraint = 0 AND NOT t.tgdeferrable \
                AND p.pronamespace = n.oid AND p.pronargs = 0 AND NOT p.proretset AND NOT p.prosecdef \
                AND p.proconfig IS NULL AND p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype \
                AND l.lanname = 'plpgsql' \
         FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class r ON r.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = r.relnamespace \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = r.oid AND a.attname = 'notification_recovery_ordinal' AND NOT a.attisdropped \
         JOIN pg_catalog.pg_proc p ON p.oid = t.tgfoid \
         JOIN pg_catalog.pg_language l ON l.oid = p.prolang \
         WHERE n.nspname = ? AND r.relname = 'pending_delivery' \
           AND t.tgname = 'pending_notification_recovery_ordinal_immutable' \
           AND p.proname = 'pending_notification_recovery_ordinal_immutable'",
        crate::db_params![schema],
    ).await?;
    let valid = match rows.next().await? {
        Some(row) => {
            row.get::<bool>(1)?
                && normalize_sql(&row.get::<String>(0)?)
                    == normalize_sql(recovery_order::POSTGRES_IMMUTABLE_BODY)
                && rows.next().await?.is_none()
        }
        None => false,
    };
    if !valid {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL immutable trigger and function",
        });
    }
    let quoted_schema = format!("\"{}\"", schema.replace('"', "\"\""));
    let mut rows = tx
        .query(
            &format!("SELECT 1 FROM {quoted_schema}.pending_delivery WHERE notification_recovery_ordinal IS NULL OR notification_recovery_ordinal <= 0 LIMIT 1"),
            (),
        )
        .await?;
    if rows.next().await?.is_some() {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "PostgreSQL ordinal values",
        });
    }
    Ok(())
}

async fn verify_sqlite_recovery(tx: &mut Transaction<'_>) -> Result<(), PendingRuntimeAttachError> {
    for (name, table, expected) in [
        (
            "pending_notification_recovery_allocate",
            "pending_delivery",
            recovery_order::SQLITE_ALLOCATE_TRIGGER,
        ),
        (
            "pending_notification_recovery_ordinal_immutable",
            "pending_delivery",
            recovery_order::SQLITE_IMMUTABLE_TRIGGER,
        ),
        (
            "pending_notification_recovery_counter_monotone",
            "pending_notification_recovery_counter",
            recovery_order::SQLITE_MONOTONE_TRIGGER,
        ),
    ] {
        let mut rows = tx.query("SELECT sql FROM main.sqlite_master WHERE type = 'trigger' AND name = ? AND tbl_name = ?", crate::db_params![name, table]).await?;
        let valid = match rows.next().await? {
            Some(row) => {
                row.get::<Option<String>>(0)?.as_deref().map(normalize_sql)
                    == Some(normalize_sql(expected))
            }
            None => false,
        };
        if !valid {
            return Err(PendingRuntimeAttachError::InvalidRecovery { component: name });
        }
    }
    let mut rows = tx.query("SELECT sql FROM main.sqlite_master WHERE type = 'table' AND name = 'pending_notification_recovery_counter'", ()).await?;
    let valid_counter_schema = match rows.next().await? {
        Some(row) => row.get::<String>(0).map(|definition| normalize_sql(&definition) == normalize_sql("CREATE TABLE pending_notification_recovery_counter (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), last_ordinal INTEGER NOT NULL CHECK (typeof(last_ordinal) = 'integer' AND last_ordinal >= 0))"))?,
        None => false,
    };
    if !valid_counter_schema {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite counter schema",
        });
    }
    let mut rows = tx
        .query(
            "SELECT sql FROM main.sqlite_master WHERE type = 'table' AND name = 'pending_delivery'",
            (),
        )
        .await?;
    let valid_ordinal_constraint = match rows.next().await? {
        Some(row) => sqlite_ordinal_declaration_is_valid(&row.get::<String>(0)?),
        None => false,
    };
    if !valid_ordinal_constraint {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite ordinal constraint",
        });
    }
    let mut rows = tx.query("SELECT singleton, last_ordinal, typeof(last_ordinal) FROM main.pending_notification_recovery_counter", ()).await?;
    let counter = match rows.next().await? {
        Some(row) => {
            let value = row.get::<i64>(1)?;
            (row.get::<i64>(0)? == 1
                && value >= 0
                && row.get::<String>(2)? == "integer"
                && rows.next().await?.is_none())
            .then_some(value)
        }
        None => None,
    };
    let counter = counter.ok_or(PendingRuntimeAttachError::InvalidRecovery {
        component: "SQLite counter singleton",
    })?;
    let mut rows = tx.query("SELECT 1 FROM main.pending_delivery WHERE notification_recovery_ordinal IS NULL OR typeof(notification_recovery_ordinal) != 'integer' OR notification_recovery_ordinal <= 0 LIMIT 1", ()).await?;
    if rows.next().await?.is_some() {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite ordinal backfill",
        });
    }
    let mut rows = tx
        .query(
            "SELECT 1 FROM main.pending_delivery WHERE notification_recovery_ordinal > ? LIMIT 1",
            crate::db_params![counter],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Err(PendingRuntimeAttachError::InvalidRecovery {
            component: "SQLite counter behind retained rows",
        });
    }
    Ok(())
}
