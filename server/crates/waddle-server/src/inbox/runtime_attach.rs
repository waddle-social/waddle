//! Read-only checks for attaching an already owner-prepared inbox.

use std::collections::HashSet;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const REQUIRED_INDEXES: [(&str, &str); 4] = [
    ("inbox_entries", "idx_inbox_entries_user_updated"),
    ("inbox_entries", "idx_inbox_entries_user_room_threads"),
    (
        "groupchat_notification_recovery",
        "idx_groupchat_notification_recovery_pending",
    ),
    (
        "groupchat_notification_recovery",
        "idx_groupchat_notification_recovery_completed_prune",
    ),
];

#[derive(Debug, thiserror::Error)]
pub enum InboxRuntimeAttachError {
    #[error("PostgreSQL inbox attachment requires a physically attested pool")]
    AttestedPostgresPoolRequired,
    #[error("inbox attachment requires the main database admission lane")]
    MainLaneRequired,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("a SQLite temporary relation shadows an inbox or recovery-ledger table")]
    SqliteTemporaryShadow,
    #[error("could not verify the PostgreSQL {setting} bound for inbox attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect inbox relation {relation}: {source}")]
    Relation {
        relation: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("inbox schema is missing the required index {index} on {table}")]
    MissingIndex {
        table: &'static str,
        index: &'static str,
    },
    #[error("inbox schema has an unexpected primary key on {table}")]
    InvalidPrimaryKey { table: &'static str },
    #[error("inbox schema requires {table}.{column} to be {requirement}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
        requirement: &'static str,
    },
    #[error("inbox runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("inbox relation {component} contains unexpected triggers or rules")]
    UnexpectedExecutableSchema { component: &'static str },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

pub(super) async fn verify(
    db: &Database,
    require_runtime_writes: bool,
) -> Result<(), InboxRuntimeAttachError> {
    if db.is_ingress_lane() {
        return Err(InboxRuntimeAttachError::MainLaneRequired);
    }
    let capability = if db.driver() == DatabaseDriver::Postgres {
        let capability = db
            .attested_pg_writer_capability()
            .ok_or(InboxRuntimeAttachError::AttestedPostgresPoolRequired)?;
        db.verify_attested_pg_writer_capability(&capability)
            .map_err(DatabaseError::from)?;
        Some(capability)
    } else {
        None
    };
    let mut tx = db.begin().await?;
    let schema = if let Some(capability) = capability {
        // Refuse an inherited transaction snapshot before opening our own
        // read-only snapshot, as in the global runtime verifier.
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
        crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
            .await
            .map_err(|error| match error {
                crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                    InboxRuntimeAttachError::InvalidTimeoutSetting { setting }
                }
                crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                    InboxRuntimeAttachError::Database(error)
                }
            })?;
        let expected = &capability.identity().schema.name.0;
        let quoted = crate::db::physical_attestation::quote_identifier(expected);
        // Take table locks before the first snapshot-forming query. Otherwise
        // an owner could commit DDL after attestation fixed the RR snapshot
        // and before these tables are locked for catalog inspection.
        for table in ["inbox_entries", "groupchat_notification_recovery"] {
            tx.execute(
                &format!("LOCK TABLE {quoted}.\"{table}\" IN ACCESS SHARE MODE"),
                (),
            )
            .await
            .map_err(|source| InboxRuntimeAttachError::Relation {
                relation: table,
                source,
            })?;
        }
        capability.attest_transaction(&mut tx).await?;
        let mut rows = tx.query("SELECT pg_catalog.current_schema()", ()).await?;
        let active: Option<String> = rows
            .next()
            .await?
            .ok_or(InboxRuntimeAttachError::ActiveSchemaMismatch)?
            .get(0)?;
        if active.as_deref() != Some(expected) {
            return Err(InboxRuntimeAttachError::ActiveSchemaMismatch);
        }
        expected.clone()
    } else {
        let mut rows = tx
            .query(
                "SELECT name FROM sqlite_temp_master WHERE type IN ('table', 'view') \
                 AND name COLLATE NOCASE IN ('inbox_entries', 'groupchat_notification_recovery', \
                     'ingress_effect_intents', 'ingress_effect_receipts', 'ingress_messages')",
                (),
            )
            .await?;
        if rows.next().await?.is_some() {
            return Err(InboxRuntimeAttachError::SqliteTemporaryShadow);
        }
        "main".to_owned()
    };
    let quoted_schema = format!("\"{}\"", schema.replace('"', "\"\""));

    for (table, columns) in [
        (
            "inbox_entries",
            "user_jid, partner_jid, thread_id, kind, last_stanza_id, last_updated, \
             unread, preview, thread_title, reply_count, author, call_thread_kind, \
             call_thread_media, call_ended_at, call_duration",
        ),
        (
            "groupchat_notification_recovery",
            "message_key, recipient_bare_jid, room_jid, thread_id, stanza_id_by, \
             stanza_id, sender_jid, is_live_occupant, room_members_only, \
             sender_can_broadcast_channel_mention, created_at_ms, completed_at_ms",
        ),
    ] {
        tx.query(
            &format!("SELECT {columns} FROM {quoted_schema}.\"{table}\" WHERE 1 = 0"),
            (),
        )
        .await
        .map_err(|source| InboxRuntimeAttachError::Relation {
            relation: table,
            source,
        })?;
    }

    verify_indexes(&mut tx, db.driver(), &schema).await?;
    verify_primary_key(
        &mut tx,
        db.driver(),
        &schema,
        "inbox_entries",
        &["user_jid", "partner_jid", "thread_id"],
    )
    .await?;
    verify_primary_key(
        &mut tx,
        db.driver(),
        &schema,
        "groupchat_notification_recovery",
        &[
            "recipient_bare_jid",
            "room_jid",
            "thread_id",
            "stanza_id_by",
            "stanza_id",
        ],
    )
    .await?;
    verify_critical_columns(&mut tx, db.driver(), &schema).await?;
    if require_runtime_writes && db.driver() != DatabaseDriver::Postgres {
        return Err(InboxRuntimeAttachError::AttestedPostgresPoolRequired);
    }
    let unexpected_schema = if require_runtime_writes {
        crate::db::runtime_store_acl::unexpected_inbox_executable_schema(&mut tx, &schema).await?
    } else {
        None
    };
    if let Some(component) = unexpected_schema {
        return Err(InboxRuntimeAttachError::UnexpectedExecutableSchema { component });
    }
    let privilege_failure = if require_runtime_writes {
        crate::db::runtime_store_acl::verify_inbox(&mut tx, &schema).await?
    } else {
        None
    };
    if let Some(component) = privilege_failure {
        return Err(InboxRuntimeAttachError::UnsafeRuntimePrivileges { component });
    }
    tx.commit().await?;
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    schema: &str,
) -> Result<(), InboxRuntimeAttachError> {
    let mut rows = match driver {
        DatabaseDriver::Postgres => {
            tx.query(
                "SELECT t.relname::text, i.relname::text \
                 FROM pg_catalog.pg_index AS x \
                 JOIN pg_catalog.pg_class AS i ON i.oid = x.indexrelid \
                 JOIN pg_catalog.pg_class AS t ON t.oid = x.indrelid \
                 JOIN pg_catalog.pg_namespace AS n ON n.oid = t.relnamespace \
                 WHERE n.nspname = ? AND x.indisvalid AND x.indisready AND x.indislive \
                   AND t.relname IN ('inbox_entries', 'groupchat_notification_recovery')",
                crate::db_params![schema],
            )
            .await?
        }
        DatabaseDriver::Sqlite => {
            tx.query(
                "SELECT tbl_name, name FROM main.sqlite_master \
                 WHERE type = 'index' AND tbl_name IN \
                 ('inbox_entries', 'groupchat_notification_recovery')",
                (),
            )
            .await?
        }
    };
    let mut actual = HashSet::new();
    while let Some(row) = rows.next().await? {
        actual.insert((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    for (table, index) in REQUIRED_INDEXES {
        if !actual.contains(&(table.to_owned(), index.to_owned())) {
            return Err(InboxRuntimeAttachError::MissingIndex { table, index });
        }
    }
    Ok(())
}

async fn verify_primary_key(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    schema: &str,
    table: &'static str,
    expected: &[&str],
) -> Result<(), InboxRuntimeAttachError> {
    let mut keys = Vec::new();
    match driver {
        DatabaseDriver::Postgres => {
            let mut rows = tx
                .query(
                    "SELECT a.attname::text \
                     FROM pg_catalog.pg_constraint AS c \
                     JOIN pg_catalog.pg_class AS t ON t.oid = c.conrelid \
                     JOIN pg_catalog.pg_namespace AS n ON n.oid = t.relnamespace \
                     CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) WITH ORDINALITY AS k(attnum, position) \
                     JOIN pg_catalog.pg_attribute AS a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                     WHERE c.contype = 'p' AND n.nspname = ? AND t.relname = ? \
                     ORDER BY k.position",
                    crate::db_params![schema, table],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                keys.push(row.get::<String>(0)?);
            }
        }
        DatabaseDriver::Sqlite => {
            // `table` is one of the two literals supplied above.
            let mut rows = tx
                .query(&format!("PRAGMA main.table_info(\"{table}\")"), ())
                .await?;
            let mut positions = Vec::new();
            while let Some(row) = rows.next().await? {
                let position: i64 = row.get(5)?;
                if position > 0 {
                    positions.push((position, row.get::<String>(1)?));
                }
            }
            positions.sort_by_key(|(position, _)| *position);
            keys.extend(positions.into_iter().map(|(_, name)| name));
        }
    }
    if keys.iter().map(String::as_str).collect::<Vec<_>>() != expected {
        return Err(InboxRuntimeAttachError::InvalidPrimaryKey { table });
    }
    Ok(())
}

async fn verify_critical_columns(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    schema: &str,
) -> Result<(), InboxRuntimeAttachError> {
    let not_null = match driver {
        DatabaseDriver::Postgres => {
            let mut rows = tx
                .query(
                    "SELECT a.attnotnull, pg_catalog.format_type(a.atttypid, a.atttypmod) \
                     FROM pg_catalog.pg_attribute AS a \
                     JOIN pg_catalog.pg_class AS t ON t.oid = a.attrelid \
                     JOIN pg_catalog.pg_namespace AS n ON n.oid = t.relnamespace \
                     WHERE n.nspname = ? AND t.relname = 'inbox_entries' \
                       AND a.attname = 'last_updated' AND NOT a.attisdropped",
                    crate::db_params![schema],
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(InboxRuntimeAttachError::InvalidColumn {
                    table: "inbox_entries",
                    column: "last_updated",
                    requirement: "a non-null bigint",
                })?;
            if !row.get::<bool>(0)? || row.get::<String>(1)? != "bigint" {
                return Err(InboxRuntimeAttachError::InvalidColumn {
                    table: "inbox_entries",
                    column: "last_updated",
                    requirement: "a non-null bigint",
                });
            }
            let mut rows = tx
                .query(
                    "SELECT a.attnotnull FROM pg_catalog.pg_attribute AS a \
                     JOIN pg_catalog.pg_class AS t ON t.oid = a.attrelid \
                     JOIN pg_catalog.pg_namespace AS n ON n.oid = t.relnamespace \
                     WHERE n.nspname = ? AND t.relname = 'groupchat_notification_recovery' \
                       AND a.attname = 'message_key' AND NOT a.attisdropped",
                    crate::db_params![schema],
                )
                .await?;
            rows.next()
                .await?
                .map(|row| row.get::<bool>(0))
                .transpose()?
                .unwrap_or(false)
        }
        DatabaseDriver::Sqlite => {
            let mut rows = tx
                .query(
                    "PRAGMA main.table_info(\"groupchat_notification_recovery\")",
                    (),
                )
                .await?;
            let mut found = false;
            while let Some(row) = rows.next().await? {
                if row.get::<String>(1)? == "message_key" {
                    found = row.get::<i64>(3)? != 0;
                }
            }
            found
        }
    };
    if !not_null {
        return Err(InboxRuntimeAttachError::InvalidColumn {
            table: "groupchat_notification_recovery",
            column: "message_key",
            requirement: "non-null",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::read_only_timeout::timeout_millis;

    #[test]
    fn postgres_timeout_display_units_and_invalid_values() {
        for (value, expected) in [
            ("0", Some(0)),
            ("200ms", Some(200)),
            ("1s", Some(1_000)),
            ("2min", Some(120_000)),
            ("3h", Some(10_800_000)),
            ("4d", Some(345_600_000)),
            ("18446744073709551615d", None),
            ("-1ms", None),
            ("1us", None),
            ("unbounded", None),
        ] {
            assert_eq!(timeout_millis(value), expected, "{value}");
        }
    }
}
