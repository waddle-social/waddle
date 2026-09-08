use super::decode::{
    decode_postgres_message_row, decode_rich_payload, decode_sqlite_message_row,
    encode_nickname_generation, encode_rich_payload,
};
use super::schema::SELECT_COLUMNS;
use super::MamDatabaseBackend;
use crate::mam::storage::tombstone::origin_id_tombstone_match;
use crate::mam::storage::{MamStorageError, StoreOutcome, TerminalTombstoneOutcome};
use crate::muc::RoomClaimFenceContext;
use jid::BareJid;
use sqlx::{PgConnection, Postgres, QueryBuilder, Sqlite, SqliteConnection, Transaction};
use tracing::debug;
use uuid::Uuid;
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload};

pub(super) async fn store_message(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<StoreOutcome, MamStorageError> {
    let rich_payload = encode_rich_payload(message)?;
    let nickname_generation = encode_nickname_generation(message.nickname_generation)?;
    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    let insert = MessageInsert {
        archive_id: &archive_id,
        archive_jid,
        message,
        rich_payload: rich_payload.as_deref(),
        nickname_generation,
    };
    match backend {
        MamDatabaseBackend::Postgres(pool) => {
            let mut conn = pool.acquire().await?;
            if let Some(id) =
                find_origin_tombstone_postgres(&mut conn, archive_jid, message).await?
            {
                return Ok(StoreOutcome::TombstoneHit(id));
            }
            insert_postgres_message_on_connection(&mut conn, insert, InsertConflict::Error).await?;
        }
        MamDatabaseBackend::Sqlite(pool) => {
            let mut conn = pool.acquire().await?;
            if let Some(id) = find_origin_tombstone_sqlite(&mut conn, archive_jid, message).await? {
                return Ok(StoreOutcome::TombstoneHit(id));
            }
            insert_sqlite_message_on_connection(&mut conn, insert, InsertConflict::Error).await?;
        }
    }
    Ok(StoreOutcome::Stored(archive_id))
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
    if !claim_fence_is_held(&mut tx, fence).await? {
        tx.rollback().await?;
        return Err(MamStorageError::NotOwner {
            entity: fence.entity.clone(),
        });
    }
    if let Some(id) = find_origin_tombstone_postgres(&mut tx, archive_jid, message).await? {
        tx.commit().await?;
        return Ok(StoreOutcome::TombstoneHit(id));
    }
    let rich_payload = encode_rich_payload(message)?;
    let nickname_generation = encode_nickname_generation(message.nickname_generation)?;
    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    insert_postgres_message_on_connection(
        &mut tx,
        MessageInsert {
            archive_id: &archive_id,
            archive_jid,
            message,
            rich_payload: rich_payload.as_deref(),
            nickname_generation,
        },
        InsertConflict::Error,
    )
    .await?;
    tx.commit().await?;
    Ok(StoreOutcome::Stored(archive_id))
}
async fn claim_fence_is_held(
    tx: &mut Transaction<'_, Postgres>,
    fence: &RoomClaimFenceContext,
) -> Result<bool, MamStorageError> {
    let entity_key = format!(
        "{}:{}",
        fence.entity.entity_type.as_db_str(),
        fence.entity.id
    );
    Ok(sqlx::query(
        "SELECT 1 FROM clustering_claims WHERE entity = $1 AND node_id = $2 AND node_epoch = $3 AND claim_epoch = $4 FOR SHARE",
    )
    .bind(&entity_key)
    .bind(&fence.owner.node_id)
    .bind(&fence.owner.node_epoch)
    .bind(fence.epoch.0)
    .fetch_optional(&mut **tx)
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
}

pub(super) async fn insert_postgres_message_on_connection(
    conn: &mut PgConnection,
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

    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
    );
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
            );
    });

    match conflict {
        InsertConflict::Error => {
            query.build().execute(conn).await?;
            Ok(Some(insert.archive_id.to_string()))
        }
        InsertConflict::DoNothing => {
            query.push(" ON CONFLICT DO NOTHING RETURNING id");
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
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
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
            );
    });

    match conflict {
        InsertConflict::Error => {
            query.build().execute(conn).await?;
            Ok(Some(insert.archive_id.to_string()))
        }
        InsertConflict::DoNothing => {
            query.push(" ON CONFLICT DO NOTHING RETURNING id");
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
) -> Result<Option<String>, sqlx::Error> {
    let Some(origin) = message.origin_id.as_ref() else {
        return Ok(None);
    };
    let mut query = QueryBuilder::<Postgres>::new("SELECT ");
    query
        .push(SELECT_COLUMNS)
        .push(" FROM mam_messages WHERE room_jid = ")
        .push_bind(archive.to_string())
        .push(" AND origin_id = ")
        .push_bind(origin.as_str())
        .push(" ORDER BY timestamp ASC, id ASC");
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
        .push(" FROM mam_messages WHERE room_jid = ")
        .push_bind(archive.to_string())
        .push(" AND origin_id = ")
        .push_bind(origin.as_str())
        .push(" ORDER BY timestamp ASC, id ASC");
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
    let encoded = encode_tombstone_payload(tombstone)?;

    // XEP-0424 §Tombstones / XEP-0425 §Tombstones: drop the body
    // entirely on tombstone. With wire-fidelity body semantics, SQL NULL
    // means "no body element" and `''` remains distinct.
    let rows = match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut builder = QueryBuilder::<Sqlite>::new(
                "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
            );
            builder
                .push_bind(encoded.as_str())
                .push(" WHERE id = ")
                .push_bind(archive_id);
            builder.build().execute(pool).await?.rows_affected()
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut builder = QueryBuilder::<Postgres>::new(
                "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
            );
            builder
                .push_bind(encoded.as_str())
                .push(" WHERE id = ")
                .push_bind(archive_id);
            builder.build().execute(pool).await?.rows_affected()
        }
    };

    debug!(
        archive_id = %archive_id,
        rows_affected = rows,
        "Replaced archived message with tombstone"
    );
    Ok(rows > 0)
}

pub(super) async fn replace_with_terminal_tombstone(
    backend: &MamDatabaseBackend,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<TerminalTombstoneOutcome, MamStorageError> {
    let encoded = encode_tombstone_payload(tombstone)?;

    loop {
        let current_rich_payload: Option<Option<String>> = match backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "SELECT rich_payload FROM mam_messages WHERE id = ",
                );
                builder.push_bind(archive_id);
                builder
                    .build_query_scalar::<Option<String>>()
                    .fetch_optional(pool)
                    .await?
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "SELECT rich_payload FROM mam_messages WHERE id = ",
                );
                builder.push_bind(archive_id);
                builder
                    .build_query_scalar::<Option<String>>()
                    .fetch_optional(pool)
                    .await?
            }
        };
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

        // Compare-and-swap the exact raw rich projection observed above. If a
        // concurrent XEP-0425 moderation wins first, its different tombstone
        // projection makes this update affect zero rows; the next iteration
        // decodes that typed terminal state and preserves its metadata.
        let rows = match backend {
            MamDatabaseBackend::Sqlite(pool) => {
                let mut builder = QueryBuilder::<Sqlite>::new(
                    "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id)
                    .push(" AND rich_payload IS ")
                    .push_bind(current_rich_payload.as_deref());
                builder.build().execute(pool).await?.rows_affected()
            }
            MamDatabaseBackend::Postgres(pool) => {
                let mut builder = QueryBuilder::<Postgres>::new(
                    "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, rich_payload = ",
                );
                builder
                    .push_bind(encoded.as_str())
                    .push(" WHERE id = ")
                    .push_bind(archive_id)
                    .push(" AND rich_payload IS NOT DISTINCT FROM ")
                    .push_bind(current_rich_payload.as_deref());
                builder.build().execute(pool).await?.rows_affected()
            }
        };
        if rows > 0 {
            debug!(
                archive_id = %archive_id,
                "Replaced live archived message with terminal tombstone"
            );
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
