use sqlx::{Connection as _, PgConnection, Row};
use thiserror::Error;

use super::schema::OBSOLETE_MAM_INDEXES;
use super::SqlxMamStorage;

/// Read-only verification failure for the exact MAM schema snapshot.
#[derive(Debug, Error)]
pub enum MamSchemaVerificationError {
    #[error("MAM schema verification requires a SQLx-tracked transaction")]
    TransactionRequired,
    #[error("MAM schema verification requires REPEATABLE READ READ ONLY")]
    TransactionMode,
    #[error("MAM schema verification requires a bounded statement timeout")]
    UnboundedTimeout,
    #[error("MAM relation, kind, owner, or table behavior differs from the expected schema")]
    RelationDrift,
    #[error("MAM columns, types, nullability, or defaults differ from the expected schema")]
    ColumnDrift,
    #[error("MAM table constraints or triggers differ from the expected schema")]
    ConstraintDrift,
    #[error("MAM indexes differ from the expected definitions or are not ready")]
    IndexDrift,
    #[error("an obsolete MAM origin-deduplication index name still exists")]
    ObsoleteIndex,
    #[error("MAM contains a nonpositive archive ordinal")]
    NonpositiveOrdinal,
    #[error("MAM contains a negative archive counter")]
    NegativeCounter,
    #[error("MAM contains an exhausted archive counter")]
    ExhaustedCounter,
    #[error("MAM archive counter is missing or behind a committed message")]
    MissingOrStaleCounter,
    #[error("MAM schema verification query failed")]
    Database(#[from] sqlx::Error),
}

// Name, PostgreSQL formatted type, NOT NULL, default expression.
const MESSAGE_COLUMNS: &[(&str, &str, bool, Option<&str>)] = &[
    ("id", "text", true, None),
    ("room_jid", "text", true, None),
    ("timestamp", "timestamp with time zone", true, None),
    ("from_jid", "text", true, None),
    ("to_jid", "text", true, None),
    ("body", "text", false, None),
    ("stanza_id", "text", false, None),
    ("thread_id", "text", false, None),
    ("reply_to_id", "text", false, None),
    ("reply_to_jid", "text", false, None),
    ("origin_id", "text", false, None),
    ("message_type", "text", true, Some("'normal'::text")),
    ("stanza_xml", "text", false, None),
    ("rich_payload", "text", false, None),
    ("nickname_generation", "bigint", false, None),
    ("parent_thread_id", "text", false, None),
    ("archive_seq", "bigint", true, None),
    (
        "created_at",
        "timestamp with time zone",
        true,
        Some("CURRENT_TIMESTAMP"),
    ),
];

const SEQUENCE_COLUMNS: &[(&str, &str, bool, Option<&str>)] = &[
    ("archive_jid", "text", true, None),
    ("next_seq", "bigint", true, None),
];

pub(super) struct ExpectedIndex {
    pub(super) name: &'static str,
    pub(super) table: &'static str,
    unique: bool,
    primary: bool,
    columns: &'static [&'static str],
    options: &'static [i32],
}

pub(super) const EXPECTED_MAM_INDEXES: &[ExpectedIndex] = &[
    ExpectedIndex {
        name: "mam_messages_pkey",
        table: "mam_messages",
        unique: true,
        primary: true,
        columns: &["id"],
        options: &[0],
    },
    ExpectedIndex {
        name: "mam_archive_sequences_pkey",
        table: "mam_archive_sequences",
        unique: true,
        primary: true,
        columns: &["archive_jid"],
        options: &[0],
    },
    ExpectedIndex {
        name: "idx_mam_room_timestamp",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "timestamp"],
        options: &[0, 3],
    },
    ExpectedIndex {
        name: "idx_mam_room_sender",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "from_jid", "timestamp"],
        options: &[0, 0, 3],
    },
    ExpectedIndex {
        name: "idx_mam_room_id",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "id"],
        options: &[0, 0],
    },
    ExpectedIndex {
        name: "idx_mam_room_thread",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "thread_id", "timestamp"],
        options: &[0, 0, 3],
    },
    ExpectedIndex {
        name: "idx_mam_room_reply_to",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "reply_to_id", "timestamp"],
        options: &[0, 0, 3],
    },
    ExpectedIndex {
        name: "idx_mam_room_origin",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "origin_id"],
        options: &[0, 0],
    },
    ExpectedIndex {
        name: "idx_mam_room_stanza",
        table: "mam_messages",
        unique: false,
        primary: false,
        columns: &["room_jid", "stanza_id"],
        options: &[0, 0],
    },
    ExpectedIndex {
        name: "idx_mam_room_archive_seq",
        table: "mam_messages",
        unique: true,
        primary: false,
        columns: &["room_jid", "archive_seq"],
        options: &[0, 0],
    },
];

impl SqlxMamStorage {
    /// Verify the supplied internally selected schema against a point-in-time,
    /// read-only MAM catalog and counter snapshot. No schema or data is changed.
    ///
    /// The caller must provide a SQLx-tracked, pinned `REPEATABLE READ READ
    /// ONLY` transaction with a finite `statement_timeout`, and derive the
    /// schema and owner independently from attested configuration. Verification
    /// assumes trusted writers and privileged DDL are quiescent; it cannot
    /// establish primary fencing, lost historical high-water marks, privileges,
    /// or authority to activate a runtime.
    pub async fn verify_postgres_schema_on_connection(
        conn: &mut PgConnection,
        schema: &str,
        expected_owner: &str,
    ) -> Result<(), MamSchemaVerificationError> {
        if !conn.is_in_transaction() {
            return Err(MamSchemaVerificationError::TransactionRequired);
        }
        let (isolation, read_only, timeout): (String, String, i64) = sqlx::query_as(
            "SELECT pg_catalog.current_setting('transaction_isolation'), pg_catalog.current_setting('transaction_read_only'), (SELECT setting::pg_catalog.int8 FROM pg_catalog.pg_settings WHERE name = 'statement_timeout')",
        )
        .fetch_one(&mut *conn)
        .await?;
        if isolation != "repeatable read" || read_only != "on" {
            return Err(MamSchemaVerificationError::TransactionMode);
        }
        if timeout <= 0 {
            return Err(MamSchemaVerificationError::UnboundedTimeout);
        }
        verify_mam_relations(conn, schema, expected_owner).await?;
        verify_mam_columns(conn, schema).await?;
        verify_mam_constraints(conn, schema).await?;
        verify_mam_indexes(conn, schema, expected_owner).await?;
        verify_mam_counters(conn, schema).await
    }
}

async fn verify_mam_relations(
    conn: &mut PgConnection,
    schema: &str,
    expected_owner: &str,
) -> Result<(), MamSchemaVerificationError> {
    let rows = sqlx::query(
        r#"SELECT c.relname::pg_catalog.text AS name, owner.rolname::pg_catalog.text AS owner,
                  c.relkind::pg_catalog.text AS kind, c.relpersistence::pg_catalog.text AS persistence,
                  c.relispartition OR c.relrowsecurity OR c.relforcerowsecurity OR c.relhasrules
                    OR c.relhassubclass OR EXISTS (
                        SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = c.oid
                    ) AS unsupported
           FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_roles owner ON owner.oid = c.relowner
           WHERE n.nspname = $1 AND c.relname IN ('mam_messages', 'mam_archive_sequences')"#,
    )
    .bind(schema)
    .fetch_all(conn)
    .await?;
    if rows.len() != 2 {
        return Err(MamSchemaVerificationError::RelationDrift);
    }
    for row in rows {
        if row.try_get::<String, _>("owner")? != expected_owner
            || row.try_get::<String, _>("kind")? != "r"
            || row.try_get::<String, _>("persistence")? != "p"
            || row.try_get::<bool, _>("unsupported")?
        {
            return Err(MamSchemaVerificationError::RelationDrift);
        }
    }
    Ok(())
}

async fn verify_mam_columns(
    conn: &mut PgConnection,
    schema: &str,
) -> Result<(), MamSchemaVerificationError> {
    let rows = sqlx::query(
        r#"SELECT c.relname::pg_catalog.text AS table_name, a.attname::pg_catalog.text AS name,
                  pg_catalog.format_type(a.atttypid, a.atttypmod) AS column_type,
                  a.attnotnull, a.attgenerated::pg_catalog.text AS generated,
                  a.attidentity::pg_catalog.text AS identity,
                  a.attcollation = t.typcollation AS default_collation,
                  pg_catalog.pg_get_expr(d.adbin, d.adrelid) AS default_value
           FROM pg_catalog.pg_attribute a
           JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_catalog.pg_type t ON t.oid = a.atttypid
           LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
           WHERE n.nspname = $1 AND c.relname IN ('mam_messages', 'mam_archive_sequences')
             AND a.attnum > 0 AND NOT a.attisdropped"#,
    )
    .bind(schema)
    .fetch_all(conn)
    .await?;
    if rows.len() != MESSAGE_COLUMNS.len() + SEQUENCE_COLUMNS.len() {
        return Err(MamSchemaVerificationError::ColumnDrift);
    }
    for row in rows {
        let table: String = row.try_get("table_name")?;
        let name: String = row.try_get("name")?;
        let columns = match table.as_str() {
            "mam_messages" => MESSAGE_COLUMNS,
            "mam_archive_sequences" => SEQUENCE_COLUMNS,
            _ => return Err(MamSchemaVerificationError::ColumnDrift),
        };
        let Some((_, kind, not_null, default)) =
            columns.iter().find(|(column, ..)| *column == name)
        else {
            return Err(MamSchemaVerificationError::ColumnDrift);
        };
        if row.try_get::<String, _>("column_type")? != *kind
            || row.try_get::<bool, _>("attnotnull")? != *not_null
            || row
                .try_get::<Option<String>, _>("default_value")?
                .as_deref()
                != *default
            || !row.try_get::<String, _>("generated")?.is_empty()
            || !row.try_get::<String, _>("identity")?.is_empty()
            || !row.try_get::<bool, _>("default_collation")?
        {
            return Err(MamSchemaVerificationError::ColumnDrift);
        }
    }
    Ok(())
}

async fn verify_mam_constraints(
    conn: &mut PgConnection,
    schema: &str,
) -> Result<(), MamSchemaVerificationError> {
    let rows = sqlx::query(
        r#"SELECT c.relname::pg_catalog.text AS table_name, x.conname::pg_catalog.text AS name,
                  x.contype::pg_catalog.text AS kind, x.condeferrable, x.condeferred, x.convalidated,
                  i.relname::pg_catalog.text AS index_name
           FROM pg_catalog.pg_constraint x
           JOIN pg_catalog.pg_class c ON c.oid = x.conrelid
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           LEFT JOIN pg_catalog.pg_class i ON i.oid = x.conindid
           WHERE n.nspname = $1 AND c.relname IN ('mam_messages', 'mam_archive_sequences')"#,
    )
    .bind(schema)
    .fetch_all(&mut *conn)
    .await?;
    if rows.len() != 2 {
        return Err(MamSchemaVerificationError::ConstraintDrift);
    }
    for row in rows {
        let table: String = row.try_get("table_name")?;
        let expected = match table.as_str() {
            "mam_messages" => "mam_messages_pkey",
            "mam_archive_sequences" => "mam_archive_sequences_pkey",
            _ => return Err(MamSchemaVerificationError::ConstraintDrift),
        };
        if row.try_get::<String, _>("name")? != expected
            || row.try_get::<Option<String>, _>("index_name")?.as_deref() != Some(expected)
            || row.try_get::<String, _>("kind")? != "p"
            || row.try_get::<bool, _>("condeferrable")?
            || row.try_get::<bool, _>("condeferred")?
            || !row.try_get::<bool, _>("convalidated")?
        {
            return Err(MamSchemaVerificationError::ConstraintDrift);
        }
    }
    let has_trigger: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1 FROM pg_catalog.pg_trigger g
             JOIN pg_catalog.pg_class c ON c.oid = g.tgrelid
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname IN ('mam_messages', 'mam_archive_sequences')
               AND NOT g.tgisinternal
           )"#,
    )
    .bind(schema)
    .fetch_one(conn)
    .await?;
    if has_trigger {
        return Err(MamSchemaVerificationError::ConstraintDrift);
    }
    Ok(())
}

async fn verify_mam_indexes(
    conn: &mut PgConnection,
    schema: &str,
    expected_owner: &str,
) -> Result<(), MamSchemaVerificationError> {
    let obsolete: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c
           JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = $1 AND c.relname::pg_catalog.text = ANY($2))"#,
    )
    .bind(schema)
    .bind(OBSOLETE_MAM_INDEXES)
    .fetch_one(&mut *conn)
    .await?;
    if obsolete {
        return Err(MamSchemaVerificationError::ObsoleteIndex);
    }
    let expected_names: Vec<&str> = EXPECTED_MAM_INDEXES
        .iter()
        .map(|index| index.name)
        .collect();
    let rows = sqlx::query(
        r#"SELECT idx.relname::pg_catalog.text AS name, tbl.relname::pg_catalog.text AS table_name,
                  owner.rolname::pg_catalog.text AS owner,
                  idx.relkind::pg_catalog.text AS kind, idx.relpersistence::pg_catalog.text AS persistence,
                  am.amname::pg_catalog.text AS method, i.indisvalid, i.indisready, i.indislive,
                  i.indisunique, i.indisprimary, i.indisexclusion, i.indimmediate,
                  i.indnullsnotdistinct, i.indisclustered, i.indisreplident,
                  i.indnkeyatts = i.indnatts AS no_includes,
                  i.indexprs IS NULL AND i.indpred IS NULL AND idx.reloptions IS NULL AS plain,
                  ARRAY(SELECT a.attname::pg_catalog.text
                        FROM pg_catalog.unnest(i.indkey::pg_catalog.int2[]) WITH ORDINALITY k(attnum, pos)
                        JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
                        ORDER BY k.pos) AS columns,
                  ARRAY(SELECT o.option::pg_catalog.int4
                        FROM pg_catalog.unnest(i.indoption::pg_catalog.int2[]) WITH ORDINALITY o(option, pos)
                        ORDER BY o.pos) AS options,
                  NOT EXISTS (
                    SELECT 1 FROM pg_catalog.unnest(i.indkey::pg_catalog.int2[]) WITH ORDINALITY k(attnum, pos)
                    LEFT JOIN pg_catalog.unnest(i.indclass::pg_catalog.oid[]) WITH ORDINALITY oc(opcoid, pos) ON oc.pos = k.pos
                    LEFT JOIN pg_catalog.unnest(i.indcollation::pg_catalog.oid[]) WITH ORDINALITY co(colloid, pos) ON co.pos = k.pos
                    LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
                    LEFT JOIN pg_catalog.pg_opclass op ON op.oid = oc.opcoid
                    WHERE a.attnum IS NULL OR op.oid IS NULL OR NOT op.opcdefault
                       OR op.opcintype <> a.atttypid OR co.colloid IS DISTINCT FROM a.attcollation
                  ) AS default_keys
           FROM pg_catalog.pg_index i
           JOIN pg_catalog.pg_class idx ON idx.oid = i.indexrelid
           JOIN pg_catalog.pg_class tbl ON tbl.oid = i.indrelid
           JOIN pg_catalog.pg_namespace n ON n.oid = idx.relnamespace
           JOIN pg_catalog.pg_roles owner ON owner.oid = idx.relowner
           JOIN pg_catalog.pg_am am ON am.oid = idx.relam
           WHERE n.nspname = $1 AND (tbl.relname IN ('mam_messages', 'mam_archive_sequences')
                                     OR idx.relname::pg_catalog.text = ANY($2))"#,
    )
    .bind(schema)
    .bind(&expected_names)
    .fetch_all(conn)
    .await?;
    if rows.len() != EXPECTED_MAM_INDEXES.len() {
        return Err(MamSchemaVerificationError::IndexDrift);
    }
    for row in rows {
        let name: String = row.try_get("name")?;
        let Some(expected) = EXPECTED_MAM_INDEXES.iter().find(|index| index.name == name) else {
            return Err(MamSchemaVerificationError::IndexDrift);
        };
        if row.try_get::<String, _>("table_name")? != expected.table
            || row.try_get::<String, _>("owner")? != expected_owner
            || row.try_get::<String, _>("kind")? != "i"
            || row.try_get::<String, _>("persistence")? != "p"
            || row.try_get::<String, _>("method")? != "btree"
            || row.try_get::<bool, _>("indisunique")? != expected.unique
            || row.try_get::<bool, _>("indisprimary")? != expected.primary
            || !row.try_get::<bool, _>("indisvalid")?
            || !row.try_get::<bool, _>("indisready")?
            || !row.try_get::<bool, _>("indislive")?
            || !row.try_get::<bool, _>("indimmediate")?
            || row.try_get::<bool, _>("indisexclusion")?
            || row.try_get::<bool, _>("indnullsnotdistinct")?
            || row.try_get::<bool, _>("indisclustered")?
            || row.try_get::<bool, _>("indisreplident")?
            || !row.try_get::<bool, _>("no_includes")?
            || !row.try_get::<bool, _>("plain")?
            || !row.try_get::<bool, _>("default_keys")?
            || row.try_get::<Vec<String>, _>("columns")? != expected.columns
            || row.try_get::<Vec<i32>, _>("options")? != expected.options
        {
            return Err(MamSchemaVerificationError::IndexDrift);
        }
    }
    Ok(())
}

async fn verify_mam_counters(
    conn: &mut PgConnection,
    schema: &str,
) -> Result<(), MamSchemaVerificationError> {
    let messages = super::postgres_relation(Some(schema), "mam_messages");
    let sequences = super::postgres_relation(Some(schema), "mam_archive_sequences");
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {messages} WHERE archive_seq <= 0), \
                EXISTS (SELECT 1 FROM {sequences} WHERE next_seq < 0), \
                EXISTS (SELECT 1 FROM {sequences} WHERE next_seq = 9223372036854775807), \
                EXISTS (SELECT 1 FROM {messages} m LEFT JOIN {sequences} s ON s.archive_jid = m.room_jid \
                        WHERE s.archive_jid IS NULL OR s.next_seq < m.archive_seq)"
    );
    let (bad_ordinal, negative, exhausted, missing_or_stale): (bool, bool, bool, bool) =
        sqlx::query_as(&sql).fetch_one(conn).await?;
    if bad_ordinal {
        return Err(MamSchemaVerificationError::NonpositiveOrdinal);
    }
    if negative {
        return Err(MamSchemaVerificationError::NegativeCounter);
    }
    if exhausted {
        return Err(MamSchemaVerificationError::ExhaustedCounter);
    }
    if missing_or_stale {
        return Err(MamSchemaVerificationError::MissingOrStaleCounter);
    }
    Ok(())
}
