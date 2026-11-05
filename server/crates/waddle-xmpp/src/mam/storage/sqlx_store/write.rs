use super::allocation::{allocate_postgres, allocate_sqlite};
use super::decode::{
    decode_postgres_message_row, decode_rich_payload, decode_sqlite_message_row,
    encode_nickname_generation, encode_rich_payload,
};
use super::schema::SELECT_COLUMNS;
use super::MamDatabaseBackend;
use crate::mam::storage::tombstone::origin_id_tombstone_match;
use crate::mam::storage::{MamStorageError, StoreOutcome, TerminalTombstoneOutcome};
use crate::muc::RoomClaimFenceContext;
use crate::ownership::EntityType;
use jid::BareJid;
use sqlx::{Connection as _, PgConnection, Postgres, QueryBuilder, Sqlite, SqliteConnection};
use tracing::debug;
use uuid::Uuid;
use waddle_xmpp_core::mam::{
    ArchiveOrdinal, ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload,
};

pub(super) async fn store_message(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<StoreOutcome, MamStorageError> {
    match backend {
        MamDatabaseBackend::Postgres(pool) => {
            let mut tx = pool.begin().await?;
            let outcome =
                store_message_postgres_connection(&mut tx, None, archive_jid, message).await?;
            if matches!(&outcome, StoreOutcome::TombstoneHit(_)) {
                tx.rollback().await?;
            } else {
                tx.commit().await?;
            }
            Ok(outcome)
        }
        MamDatabaseBackend::Sqlite(pool) => {
            // Acquire the write reservation before tombstone reads: a deferred
            // WAL transaction cannot upgrade a snapshot after another writer commits.
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            let outcome = store_message_sqlite_connection(&mut tx, archive_jid, message).await?;
            if matches!(&outcome, StoreOutcome::TombstoneHit(_)) {
                tx.rollback().await?;
            } else {
                tx.commit().await?;
            }
            Ok(outcome)
        }
    }
}

pub(super) async fn store_message_fenced(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    fence: &RoomClaimFenceContext,
) -> Result<StoreOutcome, MamStorageError> {
    let MamDatabaseBackend::Postgres(pool) = backend else {
        return store_message(backend, archive_jid, message).await;
    };
    let mut tx = pool.begin().await?;
    match store_message_fenced_postgres_connection(&mut tx, None, None, archive_jid, message, fence)
        .await
    {
        Ok(outcome) => {
            tx.commit().await?;
            Ok(outcome)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

impl super::SqlxMamStorage {
    /// Store a message in a caller-owned PostgreSQL transaction.
    pub async fn store_message_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        mam_schema: &str,
        archive: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        store_message_postgres_connection(conn, Some(mam_schema), archive, message).await
    }

    /// Store a message in a caller-owned SQLite transaction.
    pub async fn store_message_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive: &BareJid,
        message: &ArchivedMessage,
    ) -> Result<StoreOutcome, MamStorageError> {
        store_message_sqlite_connection(conn, archive, message).await
    }

    /// Check a claim and store its room message on one caller-owned PostgreSQL
    /// transaction. Both schema names are quoted independently.
    pub async fn store_message_fenced_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        mam_schema: &str,
        claim_schema: &str,
        archive: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        store_message_fenced_postgres_connection(
            conn,
            Some(mam_schema),
            Some(claim_schema),
            archive,
            message,
            fence,
        )
        .await
    }

    /// SQLite has no cluster claim relation, matching the pool-backed
    /// implementation's portable fallback to ordinary archive storage.
    pub async fn store_message_fenced_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive: &BareJid,
        message: &ArchivedMessage,
        fence: &RoomClaimFenceContext,
    ) -> Result<StoreOutcome, MamStorageError> {
        let _ = fence;
        store_message_sqlite_connection(conn, archive, message).await
    }
}

pub(super) async fn store_message_postgres_connection(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<StoreOutcome, MamStorageError> {
    require_transaction(conn.is_in_transaction())?;
    let rich_payload = encode_rich_payload(message)?;
    let nickname_generation = encode_nickname_generation(message.nickname_generation)?;
    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    if let Some(id) = find_origin_tombstone_postgres(conn, archive_jid, message, schema).await? {
        return Ok(StoreOutcome::TombstoneHit(id));
    }
    let ordinal = allocate_postgres(conn, archive_jid, schema).await?;
    insert_postgres_message_on_connection(
        conn,
        MessageInsert {
            archive_id: &archive_id,
            archive_jid,
            message,
            rich_payload: rich_payload.as_deref(),
            nickname_generation,
            ordinal,
        },
        InsertConflict::Error,
        schema,
    )
    .await?;
    Ok(StoreOutcome::Stored {
        stanza_id: archive_id,
        ordinal,
    })
}

pub(super) async fn store_message_sqlite_connection(
    conn: &mut SqliteConnection,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<StoreOutcome, MamStorageError> {
    require_transaction(conn.is_in_transaction())?;
    let rich_payload = encode_rich_payload(message)?;
    let nickname_generation = encode_nickname_generation(message.nickname_generation)?;
    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    if let Some(id) = find_origin_tombstone_sqlite(conn, archive_jid, message).await? {
        return Ok(StoreOutcome::TombstoneHit(id));
    }
    let ordinal = allocate_sqlite(conn, archive_jid).await?;
    insert_sqlite_message_on_connection(
        conn,
        MessageInsert {
            archive_id: &archive_id,
            archive_jid,
            message,
            rich_payload: rich_payload.as_deref(),
            nickname_generation,
            ordinal,
        },
        InsertConflict::Error,
    )
    .await?;
    Ok(StoreOutcome::Stored {
        stanza_id: archive_id,
        ordinal,
    })
}

pub(super) async fn store_message_fenced_postgres_connection(
    conn: &mut PgConnection,
    mam_schema: Option<&str>,
    claim_schema: Option<&str>,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    fence: &RoomClaimFenceContext,
) -> Result<StoreOutcome, MamStorageError> {
    if fence.entity.entity_type != EntityType::RoomActor
        || fence.entity.id != archive_jid.to_string()
    {
        return Err(MamStorageError::NotOwner {
            entity: fence.entity.clone(),
        });
    }
    require_transaction(conn.is_in_transaction())?;
    if !claim_fence_is_held(conn, claim_schema, fence).await? {
        return Err(MamStorageError::NotOwner {
            entity: fence.entity.clone(),
        });
    }
    store_message_postgres_connection(conn, mam_schema, archive_jid, message).await
}

fn require_transaction(in_transaction: bool) -> Result<(), MamStorageError> {
    if in_transaction {
        Ok(())
    } else {
        Err(MamStorageError::Database(
            "MAM write requires a caller-owned transaction".to_owned(),
        ))
    }
}

async fn claim_fence_is_held(
    conn: &mut PgConnection,
    claim_schema: Option<&str>,
    fence: &RoomClaimFenceContext,
) -> Result<bool, MamStorageError> {
    let entity_key = format!(
        "{}:{}",
        fence.entity.entity_type.as_db_str(),
        fence.entity.id
    );
    let relation = super::postgres_relation(claim_schema, "clustering_claims");
    let query = format!(
        "SELECT 1 FROM {relation} WHERE entity = $1 AND node_id = $2 AND node_epoch = $3 AND claim_epoch = $4 FOR SHARE"
    );
    Ok(sqlx::query(&query)
        .bind(&entity_key)
        .bind(&fence.owner.node_id)
        .bind(&fence.owner.node_epoch)
        .bind(fence.epoch.0)
        .fetch_optional(conn)
        .await?
        .is_some())
}

pub(super) enum InsertConflict {
    Error,
    DoNothing,
}

pub(super) struct MessageInsert<'a> {
    pub archive_id: &'a str,
    pub archive_jid: &'a BareJid,
    pub message: &'a ArchivedMessage,
    pub rich_payload: Option<&'a str>,
    pub nickname_generation: Option<i64>,
    pub ordinal: ArchiveOrdinal,
}

pub(super) async fn insert_postgres_message_on_connection(
    conn: &mut PgConnection,
    insert: MessageInsert<'_>,
    conflict: InsertConflict,
    schema: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    let archive_jid_str = insert.archive_jid.to_string();
    let from_jid_str = insert.message.from.to_string();
    let to_jid_str = insert.message.to.to_string();
    let reply_to_id = insert.message.reply.as_ref().map(|reply| reply.id.as_str());
    let reply_to_jid = insert
        .message
        .reply
        .as_ref()
        .and_then(|reply| reply.to.as_ref())
        .map(jid::Jid::to_string);
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&insert.message.message_type);

    let mut query = QueryBuilder::<Postgres>::new(format!(
        "INSERT INTO {} (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, archive_seq) ",
        super::postgres_relation(schema, "mam_messages")
    ));
    query.push_values(std::iter::once(()), |mut builder, _| {
        builder
            .push_bind(insert.archive_id)
            .push_bind(archive_jid_str.as_str())
            .push_bind(insert.message.timestamp)
            .push_bind(from_jid_str.as_str())
            .push_bind(to_jid_str.as_str())
            .push_bind(insert.message.body.as_deref())
            .push_bind(
                insert
                    .message
                    .stanza_id
                    .as_ref()
                    .map(|stanza_id| stanza_id.id.as_str()),
            )
            .push_bind(
                insert
                    .message
                    .thread
                    .as_ref()
                    .map(|thread| thread.id.as_str()),
            )
            .push_bind(reply_to_id)
            .push_bind(reply_to_jid.as_deref())
            .push_bind(
                insert
                    .message
                    .origin_id
                    .as_ref()
                    .map(|origin_id| origin_id.id.as_str()),
            )
            .push_bind(message_type)
            .push_bind(insert.message.stanza_xml.as_deref())
            .push_bind(insert.rich_payload)
            .push_bind(insert.nickname_generation)
            .push_bind(
                insert
                    .message
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.parent.as_ref())
                    .map(|parent| parent.as_str()),
            )
            .push_bind(insert.ordinal.to_storage());
    });

    match conflict {
        InsertConflict::Error => {
            query.build().execute(conn).await?;
            Ok(Some(insert.archive_id.to_string()))
        }
        InsertConflict::DoNothing => {
            query.push(" ON CONFLICT (id) DO NOTHING RETURNING id");
            query
                .build_query_scalar::<String>()
                .fetch_optional(conn)
                .await
        }
    }
}

pub(super) async fn insert_sqlite_message_on_connection(
    conn: &mut SqliteConnection,
    insert: MessageInsert<'_>,
    conflict: InsertConflict,
) -> Result<Option<String>, sqlx::Error> {
    let archive_jid_str = insert.archive_jid.to_string();
    let from_jid_str = insert.message.from.to_string();
    let to_jid_str = insert.message.to.to_string();
    let reply_to_id = insert.message.reply.as_ref().map(|reply| reply.id.as_str());
    let reply_to_jid = insert
        .message
        .reply
        .as_ref()
        .and_then(|reply| reply.to.as_ref())
        .map(jid::Jid::to_string);
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&insert.message.message_type);

    let mut query = QueryBuilder::<Sqlite>::new(
        "INSERT INTO main.mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, archive_seq) ",
    );
    query.push_values(std::iter::once(()), |mut builder, _| {
        builder
            .push_bind(insert.archive_id)
            .push_bind(archive_jid_str.as_str())
            .push_bind(insert.message.timestamp.to_rfc3339())
            .push_bind(from_jid_str.as_str())
            .push_bind(to_jid_str.as_str())
            .push_bind(insert.message.body.as_deref())
            .push_bind(
                insert
                    .message
                    .stanza_id
                    .as_ref()
                    .map(|stanza_id| stanza_id.id.as_str()),
            )
            .push_bind(
                insert
                    .message
                    .thread
                    .as_ref()
                    .map(|thread| thread.id.as_str()),
            )
            .push_bind(reply_to_id)
            .push_bind(reply_to_jid.as_deref())
            .push_bind(
                insert
                    .message
                    .origin_id
                    .as_ref()
                    .map(|origin_id| origin_id.id.as_str()),
            )
            .push_bind(message_type)
            .push_bind(insert.message.stanza_xml.as_deref())
            .push_bind(insert.rich_payload)
            .push_bind(insert.nickname_generation)
            .push_bind(
                insert
                    .message
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.parent.as_ref())
                    .map(|parent| parent.as_str()),
            )
            .push_bind(insert.ordinal.to_storage());
    });

    match conflict {
        InsertConflict::Error => {
            query.build().execute(conn).await?;
            Ok(Some(insert.archive_id.to_string()))
        }
        InsertConflict::DoNothing => {
            query.push(" ON CONFLICT (id) DO NOTHING RETURNING id");
            query
                .build_query_scalar::<String>()
                .fetch_optional(conn)
                .await
        }
    }
}

pub(super) async fn find_origin_tombstone_postgres(
    conn: &mut PgConnection,
    archive: &BareJid,
    message: &ArchivedMessage,
    schema: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    let Some(origin) = message.origin_id.as_ref() else {
        return Ok(None);
    };
    let mut query = QueryBuilder::<Postgres>::new("SELECT ");
    query
        .push(SELECT_COLUMNS)
        .push(" FROM ")
        .push(super::postgres_relation(schema, "mam_messages"))
        .push(" WHERE room_jid = ")
        .push_bind(archive.to_string())
        .push(" AND origin_id = ")
        .push_bind(origin.as_str())
        .push(" ORDER BY archive_seq ASC");
    for row in query.build().fetch_all(conn).await? {
        let candidate = decode_postgres_message_row(&row)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        if origin_id_tombstone_match(&candidate, message) {
            return Ok(Some(candidate.id));
        }
    }
    Ok(None)
}
pub(super) async fn find_origin_tombstone_sqlite(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    message: &ArchivedMessage,
) -> Result<Option<String>, sqlx::Error> {
    let Some(origin) = message.origin_id.as_ref() else {
        return Ok(None);
    };
    let mut query = QueryBuilder::<Sqlite>::new("SELECT ");
    query
        .push(SELECT_COLUMNS)
        .push(" FROM main.mam_messages WHERE room_jid = ")
        .push_bind(archive.to_string())
        .push(" AND origin_id = ")
        .push_bind(origin.as_str())
        .push(" ORDER BY archive_seq ASC");
    for row in query.build().fetch_all(conn).await? {
        let candidate = decode_sqlite_message_row(&row)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        if origin_id_tombstone_match(&candidate, message) {
            return Ok(Some(candidate.id));
        }
    }
    Ok(None)
}
pub(super) async fn replace_with_tombstone(
    backend: &MamDatabaseBackend,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<bool, MamStorageError> {
    match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut conn = pool.acquire().await?;
            replace_with_tombstone_sqlite_connection(&mut conn, archive_id, tombstone).await
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut conn = pool.acquire().await?;
            replace_with_tombstone_postgres_connection(&mut conn, None, archive_id, tombstone).await
        }
    }
}

pub(super) async fn replace_with_terminal_tombstone(
    backend: &MamDatabaseBackend,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<TerminalTombstoneOutcome, MamStorageError> {
    match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut conn = pool.acquire().await?;
            replace_with_terminal_tombstone_sqlite_connection(&mut conn, archive_id, tombstone)
                .await
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut conn = pool.acquire().await?;
            replace_with_terminal_tombstone_postgres_connection(
                &mut conn, None, archive_id, tombstone,
            )
            .await
        }
    }
}

impl super::SqlxMamStorage {
    pub async fn replace_with_tombstone_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        replace_with_tombstone_postgres_connection(conn, Some(schema), archive_id, tombstone).await
    }

    pub async fn replace_with_tombstone_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<bool, MamStorageError> {
        replace_with_tombstone_sqlite_connection(conn, archive_id, tombstone).await
    }

    pub async fn replace_with_terminal_tombstone_on_postgres_connection_in_schema(
        conn: &mut PgConnection,
        schema: &str,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        replace_with_terminal_tombstone_postgres_connection(
            conn,
            Some(schema),
            archive_id,
            tombstone,
        )
        .await
    }

    pub async fn replace_with_terminal_tombstone_on_sqlite_connection(
        conn: &mut SqliteConnection,
        archive_id: &str,
        tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
    ) -> Result<TerminalTombstoneOutcome, MamStorageError> {
        replace_with_terminal_tombstone_sqlite_connection(conn, archive_id, tombstone).await
    }
}

pub(super) async fn replace_with_tombstone_postgres_connection(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<bool, MamStorageError> {
    let encoded = encode_tombstone_payload(tombstone)?;
    let relation = super::postgres_relation(schema, "mam_messages");
    let mut builder = QueryBuilder::<Postgres>::new(format!(
        "UPDATE {relation} SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = "
    ));
    builder
        .push_bind(encoded.as_str())
        .push(" WHERE id = ")
        .push_bind(archive_id);
    let rows = builder.build().execute(conn).await?.rows_affected();
    debug!(archive_id = %archive_id, rows_affected = rows, "Replaced archived message with tombstone");
    Ok(rows > 0)
}

pub(super) async fn replace_with_tombstone_sqlite_connection(
    conn: &mut SqliteConnection,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<bool, MamStorageError> {
    let encoded = encode_tombstone_payload(tombstone)?;
    let mut builder = QueryBuilder::<Sqlite>::new(
        "UPDATE main.mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
    );
    builder
        .push_bind(encoded.as_str())
        .push(" WHERE id = ")
        .push_bind(archive_id);
    let rows = builder.build().execute(conn).await?.rows_affected();
    debug!(archive_id = %archive_id, rows_affected = rows, "Replaced archived message with tombstone");
    Ok(rows > 0)
}

pub(super) async fn replace_with_terminal_tombstone_postgres_connection(
    conn: &mut PgConnection,
    schema: Option<&str>,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<TerminalTombstoneOutcome, MamStorageError> {
    let encoded = encode_tombstone_payload(tombstone)?;
    let relation = super::postgres_relation(schema, "mam_messages");
    loop {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT rich_payload FROM {relation} WHERE id = "
        ));
        query.push_bind(archive_id);
        let current_rich_payload = query
            .build_query_scalar::<Option<String>>()
            .fetch_optional(&mut *conn)
            .await?;
        let Some(current_rich_payload) = current_rich_payload else {
            return Ok(TerminalTombstoneOutcome::NotFound);
        };
        let current_rich = decode_rich_payload(current_rich_payload.as_deref())?;
        if current_rich
            .as_ref()
            .is_some_and(ArchivedRichMessage::is_tombstoned)
        {
            return Ok(TerminalTombstoneOutcome::AlreadyTombstoned);
        }

        let mut update = QueryBuilder::<Postgres>::new(format!(
            "UPDATE {relation} SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = "
        ));
        update
            .push_bind(encoded.as_str())
            .push(" WHERE id = ")
            .push_bind(archive_id)
            .push(" AND rich_payload IS NOT DISTINCT FROM ")
            .push_bind(current_rich_payload.as_deref());
        if update.build().execute(&mut *conn).await?.rows_affected() > 0 {
            debug!(archive_id = %archive_id, "Replaced live archived message with terminal tombstone");
            return Ok(TerminalTombstoneOutcome::Replaced);
        }
    }
}

pub(super) async fn replace_with_terminal_tombstone_sqlite_connection(
    conn: &mut SqliteConnection,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<TerminalTombstoneOutcome, MamStorageError> {
    let encoded = encode_tombstone_payload(tombstone)?;
    loop {
        let mut query =
            QueryBuilder::<Sqlite>::new("SELECT rich_payload FROM main.mam_messages WHERE id = ");
        query.push_bind(archive_id);
        let current_rich_payload = query
            .build_query_scalar::<Option<String>>()
            .fetch_optional(&mut *conn)
            .await?;
        let Some(current_rich_payload) = current_rich_payload else {
            return Ok(TerminalTombstoneOutcome::NotFound);
        };
        let current_rich = decode_rich_payload(current_rich_payload.as_deref())?;
        if current_rich
            .as_ref()
            .is_some_and(ArchivedRichMessage::is_tombstoned)
        {
            return Ok(TerminalTombstoneOutcome::AlreadyTombstoned);
        }

        let mut update = QueryBuilder::<Sqlite>::new(
            "UPDATE main.mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
        );
        update
            .push_bind(encoded.as_str())
            .push(" WHERE id = ")
            .push_bind(archive_id)
            .push(" AND rich_payload IS ")
            .push_bind(current_rich_payload.as_deref());
        if update.build().execute(&mut *conn).await?.rows_affected() > 0 {
            debug!(archive_id = %archive_id, "Replaced live archived message with terminal tombstone");
            return Ok(TerminalTombstoneOutcome::Replaced);
        }
    }
}

fn encode_tombstone_payload(
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<String, MamStorageError> {
    let payload = ArchivedRichMessage {
        payload: Some(ArchivedRichPayload::Tombstone(tombstone)),
        reply: None,
        references: Vec::new(),
        mentions: Vec::new(),
        subjects: Default::default(),
        // XEP-0424 §Tombstones: the occupant-id and real-JID item
        // identify the original sender and MUST NOT survive the
        // tombstone replacement.
        occupant_id: None,
        muc_sender: None,
    };
    serde_json::to_string(&payload)
        .map_err(|error| MamStorageError::Serialization(error.to_string()))
}
