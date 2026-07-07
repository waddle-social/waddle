use std::str::FromStr;

use jid::{BareJid, Jid};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{Postgres, QueryBuilder, Row, Sqlite};
use tracing::{debug, warn};
use uuid::Uuid;
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload};
use xmpp_parsers::message::MessageType;

use crate::mam::storage::MamStorageError;
use crate::muc::RoomClaimFenceContext;

use super::decode::{encode_nickname_generation, encode_rich_payload};
use super::MamDatabaseBackend;

pub(super) async fn store_message(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<String, MamStorageError> {
    let rich_payload = encode_rich_payload(message)?;
    let origin_dedup_fingerprint = message
        .origin_id
        .as_ref()
        .map(|_| origin_dedup_fingerprint(message, rich_payload.as_deref()));

    if let Some(existing_archive_id) = find_existing_origin_id_match(
        backend,
        archive_jid,
        message,
        rich_payload.as_deref(),
        origin_dedup_fingerprint.as_deref(),
    )
    .await?
    {
        return Ok(existing_archive_id);
    }

    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    // Typed-payloads boundary: convert the closed `MessageType`
    // enum to its canonical wire literal exactly once, here at
    // the SQL bind site.
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&message.message_type);
    let nickname_generation = encode_nickname_generation(message.nickname_generation)?;

    let archive_jid_str = archive_jid.to_string();
    let from_jid_str = message.from.to_string();
    let to_jid_str = message.to.to_string();
    let reply_to_id_bind = message.reply.as_ref().map(|r| r.id.as_str());
    let reply_to_jid_owned: Option<String> = message
        .reply
        .as_ref()
        .and_then(|r| r.to.as_ref())
        .map(|jid| jid.to_string());

    match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, origin_dedup_fingerprint) ",
            );
            query.push_values(std::iter::once(()), |mut builder, _| {
                builder
                    .push_bind(&archive_id)
                    .push_bind(archive_jid_str.as_str())
                    .push_bind(message.timestamp.to_rfc3339())
                    .push_bind(from_jid_str.as_str())
                    .push_bind(to_jid_str.as_str())
                    .push_bind(message.body.as_deref())
                    .push_bind(message.stanza_id.as_ref().map(|s| s.id.as_str()))
                    .push_bind(message.thread.as_ref().map(|t| t.id.as_str()))
                    .push_bind(reply_to_id_bind)
                    .push_bind(reply_to_jid_owned.as_deref())
                    .push_bind(message.origin_id.as_ref().map(|o| o.id.as_str()))
                    .push_bind(message_type)
                    .push_bind(message.stanza_xml.as_deref())
                    .push_bind(rich_payload.as_deref())
                    .push_bind(nickname_generation)
                    .push_bind(
                        message
                            .thread
                            .as_ref()
                            .and_then(|t| t.parent.as_ref())
                            .map(|p| p.as_str()),
                    )
                    .push_bind(origin_dedup_fingerprint.as_deref());
            });
            if let Err(error) = query.build().execute(pool).await {
                if let Some(existing_archive_id) = find_existing_origin_id_match(
                    backend,
                    archive_jid,
                    message,
                    rich_payload.as_deref(),
                    origin_dedup_fingerprint.as_deref(),
                )
                .await?
                {
                    return Ok(existing_archive_id);
                }
                return Err(error.into());
            }
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, origin_dedup_fingerprint) ",
            );
            query.push_values(std::iter::once(()), |mut builder, _| {
                builder
                    .push_bind(&archive_id)
                    .push_bind(archive_jid_str.as_str())
                    .push_bind(message.timestamp)
                    .push_bind(from_jid_str.as_str())
                    .push_bind(to_jid_str.as_str())
                    .push_bind(message.body.as_deref())
                    .push_bind(message.stanza_id.as_ref().map(|s| s.id.as_str()))
                    .push_bind(message.thread.as_ref().map(|t| t.id.as_str()))
                    .push_bind(reply_to_id_bind)
                    .push_bind(reply_to_jid_owned.as_deref())
                    .push_bind(message.origin_id.as_ref().map(|o| o.id.as_str()))
                    .push_bind(message_type)
                    .push_bind(message.stanza_xml.as_deref())
                    .push_bind(rich_payload.as_deref())
                    .push_bind(nickname_generation)
                    .push_bind(
                        message
                            .thread
                            .as_ref()
                            .and_then(|t| t.parent.as_ref())
                            .map(|p| p.as_str()),
                    )
                    .push_bind(origin_dedup_fingerprint.as_deref());
            });
            if let Err(error) = query.build().execute(pool).await {
                if let Some(existing_archive_id) = find_existing_origin_id_match(
                    backend,
                    archive_jid,
                    message,
                    rich_payload.as_deref(),
                    origin_dedup_fingerprint.as_deref(),
                )
                .await?
                {
                    return Ok(existing_archive_id);
                }
                return Err(error.into());
            }
        }
    }

    debug!(archive_id = %archive_id, "Message stored in MAM archive");
    Ok(archive_id)
}

/// Fenced variant of [`store_message`] (ADR-0017 Phase 3 Slice 7 FIX 1,
/// council-adjudicated): the `SELECT ... FOR SHARE` fencing check against
/// `clustering_claims` runs INSIDE the same transaction as the archive
/// insert, so a steal committing between `dispatch_to_room`'s own
/// standalone pre-fan-out check and this write can never land a phantom
/// archived row under a claim this node no longer holds.
///
/// The origin-id dedup pre-check (idempotency for retried sends) runs
/// against the plain pool, exactly as [`store_message`] already does it —
/// it only ever reads already-committed rows, so it needs no transactional
/// isolation from this write; only the fencing-check-then-insert pair needs
/// one-transaction atomicity, which this function provides. Postgres-only:
/// fencing is never enabled for a SQLite backend (clustering is
/// Postgres-only per ADR-0017 element 1 — see
/// `SqlxMamStorage::with_cluster_fencing`), so the non-Postgres arm here is
/// defensive only.
pub(super) async fn store_message_fenced(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    fence: &RoomClaimFenceContext,
) -> Result<String, MamStorageError> {
    let MamDatabaseBackend::Postgres(pool) = backend else {
        return store_message(backend, archive_jid, message).await;
    };

    let rich_payload = encode_rich_payload(message)?;
    let origin_dedup_fingerprint = message
        .origin_id
        .as_ref()
        .map(|_| origin_dedup_fingerprint(message, rich_payload.as_deref()));

    if let Some(existing_archive_id) = find_existing_origin_id_match(
        backend,
        archive_jid,
        message,
        rich_payload.as_deref(),
        origin_dedup_fingerprint.as_deref(),
    )
    .await?
    {
        return Ok(existing_archive_id);
    }

    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&message.message_type);
    let nickname_generation = encode_nickname_generation(message.nickname_generation)?;
    let archive_jid_str = archive_jid.to_string();
    let from_jid_str = message.from.to_string();
    let to_jid_str = message.to.to_string();
    let reply_to_id_bind = message.reply.as_ref().map(|r| r.id.as_str());
    let reply_to_jid_owned: Option<String> = message
        .reply
        .as_ref()
        .and_then(|r| r.to.as_ref())
        .map(|jid| jid.to_string());

    let mut tx = pool.begin().await?;

    // Fencing check: the exact `SELECT ... FOR SHARE` shape
    // `muc_durable::PostgresMucRoomStore::assert_fenced`/
    // `sm_persistence_fenced::assert_fenced`/`pending_delivery`'s
    // `insert_fenced` already establish — the first statement inside this
    // transaction, on the SAME connection as the write it guards. A failed
    // check rolls back BEFORE any write.
    let entity_key = format!(
        "{}:{}",
        fence.entity.entity_type.as_db_str(),
        fence.entity.id
    );
    let held = sqlx::query(
        "SELECT 1 FROM clustering_claims WHERE entity = $1 AND node_id = $2 AND claim_epoch = $3 FOR SHARE",
    )
    .bind(&entity_key)
    .bind(&fence.node_id)
    .bind(fence.epoch.0)
    .fetch_optional(&mut *tx)
    .await?
    .is_some();
    if !held {
        // Roll back explicitly rather than relying on drop — the fencing
        // failure is the expected, correctness-critical path here, not an
        // error worth masking behind an implicit rollback-on-drop.
        let _ = tx.rollback().await;
        return Err(MamStorageError::NotOwner {
            entity: fence.entity.clone(),
        });
    }

    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, origin_dedup_fingerprint) ",
    );
    query.push_values(std::iter::once(()), |mut builder, _| {
        builder
            .push_bind(&archive_id)
            .push_bind(archive_jid_str.as_str())
            .push_bind(message.timestamp)
            .push_bind(from_jid_str.as_str())
            .push_bind(to_jid_str.as_str())
            .push_bind(message.body.as_deref())
            .push_bind(message.stanza_id.as_ref().map(|s| s.id.as_str()))
            .push_bind(message.thread.as_ref().map(|t| t.id.as_str()))
            .push_bind(reply_to_id_bind)
            .push_bind(reply_to_jid_owned.as_deref())
            .push_bind(message.origin_id.as_ref().map(|o| o.id.as_str()))
            .push_bind(message_type)
            .push_bind(message.stanza_xml.as_deref())
            .push_bind(rich_payload.as_deref())
            .push_bind(nickname_generation)
            .push_bind(
                message
                    .thread
                    .as_ref()
                    .and_then(|t| t.parent.as_ref())
                    .map(|p| p.as_str()),
            )
            .push_bind(origin_dedup_fingerprint.as_deref());
    });
    if let Err(error) = query.build().execute(&mut *tx).await {
        let _ = tx.rollback().await;
        if let Some(existing_archive_id) = find_existing_origin_id_match(
            backend,
            archive_jid,
            message,
            rich_payload.as_deref(),
            origin_dedup_fingerprint.as_deref(),
        )
        .await?
        {
            return Ok(existing_archive_id);
        }
        return Err(error.into());
    }
    tx.commit().await?;

    debug!(archive_id = %archive_id, "Message stored in MAM archive (fenced)");
    Ok(archive_id)
}

async fn find_existing_origin_id_match(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    incoming_rich_payload: Option<&str>,
    origin_dedup_fingerprint: Option<&str>,
) -> Result<Option<String>, MamStorageError> {
    let Some(origin_id) = message.origin_id.as_ref() else {
        return Ok(None);
    };
    let archive_jid_str = archive_jid.to_string();

    match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut builder = QueryBuilder::<Sqlite>::new(
                "SELECT id, from_jid, to_jid, body, thread_id, parent_thread_id, \
                 reply_to_id, reply_to_jid, message_type, rich_payload, nickname_generation \
                 FROM mam_messages WHERE room_jid = ",
            );
            push_origin_dedup_filter(
                &mut builder,
                archive_jid_str.as_str(),
                origin_id.as_str(),
                origin_dedup_fingerprint,
            );
            let rows: Vec<SqliteRow> = builder.build().fetch_all(pool).await?;
            for row in rows {
                let Some(existing) = OriginDedupRow::from_sqlite(&row) else {
                    continue;
                };
                if existing.matches(message, incoming_rich_payload) {
                    return Ok(Some(existing.id));
                }
            }
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut builder = QueryBuilder::<Postgres>::new(
                "SELECT id, from_jid, to_jid, body, thread_id, parent_thread_id, \
                 reply_to_id, reply_to_jid, message_type, rich_payload, nickname_generation \
                 FROM mam_messages WHERE room_jid = ",
            );
            push_origin_dedup_filter(
                &mut builder,
                archive_jid_str.as_str(),
                origin_id.as_str(),
                origin_dedup_fingerprint,
            );
            let rows: Vec<PgRow> = builder.build().fetch_all(pool).await?;
            for row in rows {
                let Some(existing) = OriginDedupRow::from_postgres(&row) else {
                    continue;
                };
                if existing.matches(message, incoming_rich_payload) {
                    return Ok(Some(existing.id));
                }
            }
        }
    }

    Ok(None)
}

fn push_origin_dedup_filter<'args, DB>(
    builder: &mut QueryBuilder<'args, DB>,
    archive_jid: &'args str,
    origin_id: &'args str,
    origin_dedup_fingerprint: Option<&'args str>,
) where
    DB: sqlx::Database,
    &'args str: sqlx::Encode<'args, DB> + sqlx::Type<DB>,
{
    builder
        .push_bind(archive_jid)
        .push(" AND origin_id = ")
        .push_bind(origin_id);
    if let Some(fingerprint) = origin_dedup_fingerprint {
        builder
            .push(" AND (origin_dedup_fingerprint = ")
            .push_bind(fingerprint)
            .push(" OR origin_dedup_fingerprint IS NULL)");
    }
    builder.push(" ORDER BY timestamp ASC, id ASC");
}

struct OriginDedupRow {
    id: String,
    from: Jid,
    to: Jid,
    body: Option<String>,
    thread_id: Option<String>,
    parent_thread_id: Option<String>,
    reply_to_id: Option<String>,
    reply_to_jid: Option<Jid>,
    message_type: MessageType,
    rich_payload: Option<String>,
    nickname_generation: Option<u64>,
}

impl OriginDedupRow {
    fn from_sqlite(row: &SqliteRow) -> Option<Self> {
        Self::from_parts(OriginDedupRawRow {
            id: row.try_get("id"),
            from_raw: row.try_get("from_jid"),
            to_raw: row.try_get("to_jid"),
            body: row.try_get("body"),
            thread_id: row.try_get("thread_id"),
            parent_thread_id: row.try_get("parent_thread_id"),
            reply_to_id: row.try_get("reply_to_id"),
            reply_to_jid_raw: row.try_get("reply_to_jid"),
            message_type_raw: row.try_get("message_type"),
            rich_payload: row.try_get("rich_payload"),
            nickname_generation_raw: row.try_get::<Option<i64>, _>("nickname_generation"),
        })
    }

    fn from_postgres(row: &PgRow) -> Option<Self> {
        Self::from_parts(OriginDedupRawRow {
            id: row.try_get("id"),
            from_raw: row.try_get("from_jid"),
            to_raw: row.try_get("to_jid"),
            body: row.try_get("body"),
            thread_id: row.try_get("thread_id"),
            parent_thread_id: row.try_get("parent_thread_id"),
            reply_to_id: row.try_get("reply_to_id"),
            reply_to_jid_raw: row.try_get("reply_to_jid"),
            message_type_raw: row.try_get("message_type"),
            rich_payload: row.try_get("rich_payload"),
            nickname_generation_raw: row.try_get::<Option<i64>, _>("nickname_generation"),
        })
    }

    fn from_parts(raw: OriginDedupRawRow) -> Option<Self> {
        let id = read_dedup_column("id", raw.id)?;
        let from_raw = read_dedup_column("from_jid", raw.from_raw)?;
        let to_raw = read_dedup_column("to_jid", raw.to_raw)?;
        let body = read_dedup_column("body", raw.body)?;
        let thread_id = read_dedup_column("thread_id", raw.thread_id)?;
        let parent_thread_id = read_dedup_column("parent_thread_id", raw.parent_thread_id)?;
        let reply_to_id = read_dedup_column("reply_to_id", raw.reply_to_id)?;
        let reply_to_jid_raw = read_dedup_column("reply_to_jid", raw.reply_to_jid_raw)?;
        let message_type_raw = read_dedup_column("message_type", raw.message_type_raw)?;
        let rich_payload = read_dedup_column("rich_payload", raw.rich_payload)?;
        let nickname_generation_raw =
            read_dedup_column("nickname_generation", raw.nickname_generation_raw)?;

        let from = parse_dedup_jid(&id, "from_jid", &from_raw)?;
        let to = parse_dedup_jid(&id, "to_jid", &to_raw)?;
        let reply_to_jid = match reply_to_jid_raw.as_deref() {
            Some(raw) => Some(parse_dedup_jid(&id, "reply_to_jid", raw)?),
            None => None,
        };
        let message_type = match MessageType::from_str(message_type_raw.as_str()) {
            Ok(message_type) => message_type,
            Err(error) => {
                warn!(
                    archive_id = %id,
                    column = "message_type",
                    %error,
                    "MAM origin-id dedup candidate is malformed; skipping"
                );
                return None;
            }
        };
        let nickname_generation = match nickname_generation_raw.map(u64::try_from).transpose() {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    archive_id = %id,
                    column = "nickname_generation",
                    %error,
                    "MAM origin-id dedup candidate is malformed; skipping"
                );
                return None;
            }
        };

        Some(Self {
            id,
            from,
            to,
            body,
            thread_id,
            parent_thread_id,
            reply_to_id,
            reply_to_jid,
            message_type,
            rich_payload,
            nickname_generation,
        })
    }

    fn matches(&self, incoming: &ArchivedMessage, incoming_rich_payload: Option<&str>) -> bool {
        self.sender_scope_matches(incoming)
            && self.body.as_deref() == incoming.body.as_deref()
            && self.thread_id.as_deref()
                == incoming.thread.as_ref().map(|thread| thread.id.as_str())
            && self.parent_thread_id.as_deref()
                == incoming
                    .thread
                    .as_ref()
                    .and_then(|thread| thread.parent.as_ref())
                    .map(|parent| parent.as_str())
            && self.reply_to_id.as_deref() == incoming.reply.as_ref().map(|reply| reply.id.as_str())
            && self.reply_to_jid.as_ref()
                == incoming.reply.as_ref().and_then(|reply| reply.to.as_ref())
            && self.message_type == incoming.message_type
            && self.rich_payload.as_deref() == incoming_rich_payload
    }

    fn sender_scope_matches(&self, incoming: &ArchivedMessage) -> bool {
        match (&self.message_type, &incoming.message_type) {
            (MessageType::Groupchat, MessageType::Groupchat) => {
                self.from == incoming.from
                    && self.nickname_generation == incoming.nickname_generation
            }
            (MessageType::Groupchat, _) | (_, MessageType::Groupchat) => false,
            _ => {
                self.from.to_bare() == incoming.from.to_bare()
                    && self.to.to_bare() == incoming.to.to_bare()
            }
        }
    }
}

struct OriginDedupRawRow {
    id: Result<String, sqlx::Error>,
    from_raw: Result<String, sqlx::Error>,
    to_raw: Result<String, sqlx::Error>,
    body: Result<Option<String>, sqlx::Error>,
    thread_id: Result<Option<String>, sqlx::Error>,
    parent_thread_id: Result<Option<String>, sqlx::Error>,
    reply_to_id: Result<Option<String>, sqlx::Error>,
    reply_to_jid_raw: Result<Option<String>, sqlx::Error>,
    message_type_raw: Result<String, sqlx::Error>,
    rich_payload: Result<Option<String>, sqlx::Error>,
    nickname_generation_raw: Result<Option<i64>, sqlx::Error>,
}

fn read_dedup_column<T>(column: &'static str, result: Result<T, sqlx::Error>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            warn!(
                column,
                %error,
                "MAM origin-id dedup candidate could not be read; skipping"
            );
            None
        }
    }
}

fn parse_dedup_jid(archive_id: &str, column: &'static str, value: &str) -> Option<Jid> {
    match value.parse::<Jid>() {
        Ok(jid) => Some(jid),
        Err(error) => {
            warn!(
                archive_id,
                column,
                %error,
                "MAM origin-id dedup candidate is malformed; skipping"
            );
            None
        }
    }
}

fn origin_dedup_fingerprint(message: &ArchivedMessage, rich_payload: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    update_hash_part(
        &mut hasher,
        "message_type",
        Some(waddle_xmpp_core::mam::message_type_wire_str(
            &message.message_type,
        )),
    );
    update_hash_part(&mut hasher, "body", message.body.as_deref());
    update_hash_part(
        &mut hasher,
        "thread_id",
        message.thread.as_ref().map(|thread| thread.id.as_str()),
    );
    update_hash_part(
        &mut hasher,
        "parent_thread_id",
        message
            .thread
            .as_ref()
            .and_then(|thread| thread.parent.as_ref())
            .map(|parent| parent.as_str()),
    );
    update_hash_part(
        &mut hasher,
        "reply_to_id",
        message.reply.as_ref().map(|reply| reply.id.as_str()),
    );
    let reply_to_jid = message
        .reply
        .as_ref()
        .and_then(|reply| reply.to.as_ref())
        .map(|jid| jid.to_string());
    update_hash_part(&mut hasher, "reply_to_jid", reply_to_jid.as_deref());
    update_hash_part(&mut hasher, "rich_payload", rich_payload);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to String never fails");
    }
    hex
}

fn update_hash_part(hasher: &mut Sha256, label: &str, value: Option<&str>) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.len().to_le_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

pub(super) async fn replace_with_tombstone(
    backend: &MamDatabaseBackend,
    archive_id: &str,
    tombstone: waddle_xmpp_core::mam::ArchivedTombstone,
) -> Result<bool, MamStorageError> {
    let payload = ArchivedRichMessage {
        payload: Some(ArchivedRichPayload::Tombstone(tombstone)),
        reply: None,
        references: Vec::new(),
        mentions: Vec::new(),
    };
    let encoded = serde_json::to_string(&payload)
        .map_err(|error| MamStorageError::Serialization(error.to_string()))?;

    // XEP-0424 §Tombstones / XEP-0425 §Tombstones: drop the body
    // entirely on tombstone. With wire-fidelity body semantics, SQL NULL
    // means "no body element" and `''` remains distinct.
    let rows = match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut builder = QueryBuilder::<Sqlite>::new(
                "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, origin_dedup_fingerprint = NULL, rich_payload = ",
            );
            builder
                .push_bind(encoded.as_str())
                .push(" WHERE id = ")
                .push_bind(archive_id);
            builder.build().execute(pool).await?.rows_affected()
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut builder = QueryBuilder::<Postgres>::new(
                "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, origin_dedup_fingerprint = NULL, rich_payload = ",
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
