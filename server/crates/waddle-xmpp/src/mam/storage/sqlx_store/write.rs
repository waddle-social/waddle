use jid::BareJid;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use sqlx::{Postgres, QueryBuilder, Sqlite, Transaction};
use tracing::{debug, warn};
use uuid::Uuid;
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload};
use xmpp_parsers::message::MessageType;

use crate::mam::storage::origin_dedup::{origin_id_dedup_match, origin_id_tombstone_match};
use crate::mam::storage::{MamStorageError, StoreOutcome};
use crate::muc::RoomClaimFenceContext;

use super::decode::{
    decode_postgres_message_row, decode_sqlite_message_row, encode_nickname_generation,
    encode_rich_payload,
};
use super::schema::SELECT_COLUMNS;
use super::MamDatabaseBackend;

pub(super) async fn store_message(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<StoreOutcome, MamStorageError> {
    let rich_payload = encode_rich_payload(message)?;
    let origin_dedup_fingerprint = message
        .origin_id
        .as_ref()
        .map(|_| origin_dedup_fingerprint(message));
    let origin_dedup_sender_scope = origin_dedup_sender_scope(message);

    if let Some(outcome) = find_existing_origin_id_match(
        backend,
        archive_jid,
        message,
        origin_dedup_fingerprint.as_deref(),
    )
    .await?
    {
        return Ok(outcome);
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
                "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, origin_dedup_sender_scope, origin_dedup_fingerprint) ",
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
                    .push_bind(origin_dedup_sender_scope.as_deref())
                    .push_bind(origin_dedup_fingerprint.as_deref());
            });
            if let Err(error) = query.build().execute(pool).await {
                if let Some(outcome) = find_existing_origin_id_match(
                    backend,
                    archive_jid,
                    message,
                    origin_dedup_fingerprint.as_deref(),
                )
                .await?
                {
                    return Ok(outcome);
                }
                return Err(error.into());
            }
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, origin_dedup_sender_scope, origin_dedup_fingerprint) ",
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
                    .push_bind(origin_dedup_sender_scope.as_deref())
                    .push_bind(origin_dedup_fingerprint.as_deref());
            });
            if let Err(error) = query.build().execute(pool).await {
                if let Some(outcome) = find_existing_origin_id_match(
                    backend,
                    archive_jid,
                    message,
                    origin_dedup_fingerprint.as_deref(),
                )
                .await?
                {
                    return Ok(outcome);
                }
                return Err(error.into());
            }
        }
    }

    debug!(archive_id = %archive_id, "Message stored in MAM archive");
    Ok(StoreOutcome::Stored(archive_id))
}

/// Fenced variant of [`store_message`] (ADR-0017 Phase 3 Slice 7 FIX 1,
/// council-adjudicated): the `SELECT ... FOR SHARE` fencing check against
/// `clustering_claims` runs INSIDE the same transaction as the archive
/// insert, so a steal committing between `dispatch_to_room`'s own
/// standalone pre-fan-out check and this write can never land a phantom
/// archived row under a claim this node no longer holds.
///
/// Origin-id deduplication runs only after the claim check and inside the
/// same transaction. An idempotent retry is still an ownership-sensitive
/// archive operation: returning an existing row to a deposed actor would
/// otherwise authorize its subsequent fan-out without proving the room
/// claim. Postgres-only: fencing is never enabled for a SQLite backend
/// (clustering is Postgres-only per ADR-0017 element 1 — see
/// `SqlxMamStorage::with_cluster_fencing`), so the non-Postgres arm here is
/// defensive only.
pub(super) async fn store_message_fenced(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    fence: &RoomClaimFenceContext,
) -> Result<StoreOutcome, MamStorageError> {
    let MamDatabaseBackend::Postgres(pool) = backend else {
        return store_message(backend, archive_jid, message).await;
    };

    let rich_payload = encode_rich_payload(message)?;
    let origin_dedup_fingerprint = message
        .origin_id
        .as_ref()
        .map(|_| origin_dedup_fingerprint(message));
    let origin_dedup_sender_scope = origin_dedup_sender_scope(message);

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
    if !claim_fence_is_held(&mut tx, fence).await? {
        // Roll back explicitly rather than relying on drop — the fencing
        // failure is the expected, correctness-critical path here, not an
        // error worth masking behind an implicit rollback-on-drop.
        let _ = tx.rollback().await;
        return Err(MamStorageError::NotOwner {
            entity: fence.entity.clone(),
        });
    }
    if let Some(outcome) = find_existing_origin_id_match_postgres_tx(
        &mut tx,
        archive_jid,
        message,
        origin_dedup_fingerprint.as_deref(),
    )
    .await?
    {
        tx.commit().await?;
        return Ok(outcome);
    }

    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id, origin_dedup_sender_scope, origin_dedup_fingerprint) ",
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
            .push_bind(origin_dedup_sender_scope.as_deref())
            .push_bind(origin_dedup_fingerprint.as_deref());
    });
    if let Err(error) = query.build().execute(&mut *tx).await {
        let _ = tx.rollback().await;
        // A concurrent origin-id insert can still win after our pre-check.
        // Re-prove ownership in a fresh transaction before accepting its
        // row as the idempotent result; never fall back to a pool-level
        // dedup read that could bypass fencing after a claim transfer.
        let mut dedup_tx = pool.begin().await?;
        if !claim_fence_is_held(&mut dedup_tx, fence).await? {
            let _ = dedup_tx.rollback().await;
            return Err(MamStorageError::NotOwner {
                entity: fence.entity.clone(),
            });
        }
        let existing_outcome = find_existing_origin_id_match_postgres_tx(
            &mut dedup_tx,
            archive_jid,
            message,
            origin_dedup_fingerprint.as_deref(),
        )
        .await?;
        dedup_tx.commit().await?;
        if let Some(outcome) = existing_outcome {
            return Ok(outcome);
        }
        return Err(error.into());
    }
    tx.commit().await?;

    debug!(archive_id = %archive_id, "Message stored in MAM archive (fenced)");
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

async fn find_existing_origin_id_match_postgres_tx(
    tx: &mut Transaction<'_, Postgres>,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    origin_dedup_fingerprint: Option<&str>,
) -> Result<Option<StoreOutcome>, MamStorageError> {
    let Some(origin_id) = message.origin_id.as_ref() else {
        return Ok(None);
    };
    let archive_jid_str = archive_jid.to_string();
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&message.message_type);
    let from_jid = message.from.to_string();
    let groupchat_from =
        matches!(message.message_type, MessageType::Groupchat).then_some(from_jid.as_str());
    let mut builder = QueryBuilder::<Postgres>::new("SELECT ");
    builder
        .push(SELECT_COLUMNS)
        .push(" FROM mam_messages WHERE room_jid = ");
    push_origin_dedup_filter(
        &mut builder,
        archive_jid_str.as_str(),
        origin_id.as_str(),
        message_type,
        groupchat_from,
        origin_dedup_fingerprint,
    );
    let rows: Vec<PgRow> = builder.build().fetch_all(&mut **tx).await?;
    let candidates = rows
        .iter()
        .filter_map(|row| match decode_postgres_message_row(row) {
            Ok(candidate) => Some(candidate),
            Err(error) => {
                warn!(%error, "MAM origin-id dedup candidate is malformed; skipping");
                None
            }
        })
        .collect::<Vec<_>>();
    Ok(matching_store_outcome(&candidates, message))
}

async fn find_existing_origin_id_match(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
    origin_dedup_fingerprint: Option<&str>,
) -> Result<Option<StoreOutcome>, MamStorageError> {
    let Some(origin_id) = message.origin_id.as_ref() else {
        return Ok(None);
    };
    let archive_jid_str = archive_jid.to_string();
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&message.message_type);
    let from_jid = message.from.to_string();
    let groupchat_from =
        matches!(message.message_type, MessageType::Groupchat).then_some(from_jid.as_str());

    match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut builder = QueryBuilder::<Sqlite>::new("SELECT ");
            builder
                .push(SELECT_COLUMNS)
                .push(" FROM mam_messages WHERE room_jid = ");
            push_origin_dedup_filter(
                &mut builder,
                archive_jid_str.as_str(),
                origin_id.as_str(),
                message_type,
                groupchat_from,
                origin_dedup_fingerprint,
            );
            let rows: Vec<SqliteRow> = builder.build().fetch_all(pool).await?;
            let candidates = rows
                .iter()
                .filter_map(|row| match decode_sqlite_message_row(row) {
                    Ok(candidate) => Some(candidate),
                    Err(error) => {
                        warn!(%error, "MAM origin-id dedup candidate is malformed; skipping");
                        None
                    }
                })
                .collect::<Vec<_>>();
            Ok(matching_store_outcome(&candidates, message))
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut builder = QueryBuilder::<Postgres>::new("SELECT ");
            builder
                .push(SELECT_COLUMNS)
                .push(" FROM mam_messages WHERE room_jid = ");
            push_origin_dedup_filter(
                &mut builder,
                archive_jid_str.as_str(),
                origin_id.as_str(),
                message_type,
                groupchat_from,
                origin_dedup_fingerprint,
            );
            let rows: Vec<PgRow> = builder.build().fetch_all(pool).await?;
            let candidates = rows
                .iter()
                .filter_map(|row| match decode_postgres_message_row(row) {
                    Ok(candidate) => Some(candidate),
                    Err(error) => {
                        warn!(%error, "MAM origin-id dedup candidate is malformed; skipping");
                        None
                    }
                })
                .collect::<Vec<_>>();
            Ok(matching_store_outcome(&candidates, message))
        }
    }
}

fn matching_store_outcome(
    candidates: &[ArchivedMessage],
    incoming: &ArchivedMessage,
) -> Option<StoreOutcome> {
    if let Some(existing) = candidates
        .iter()
        .find(|existing| origin_id_dedup_match(existing, incoming))
    {
        return Some(StoreOutcome::Deduplicated(existing.id.clone()));
    }
    candidates
        .iter()
        .find(|existing| origin_id_tombstone_match(existing, incoming))
        .map(|existing| StoreOutcome::TombstoneHit(existing.id.clone()))
}

fn push_origin_dedup_filter<'args, DB>(
    builder: &mut QueryBuilder<'args, DB>,
    archive_jid: &'args str,
    origin_id: &'args str,
    message_type: &'args str,
    groupchat_from: Option<&'args str>,
    origin_dedup_fingerprint: Option<&'args str>,
) where
    DB: sqlx::Database,
    &'args str: sqlx::Encode<'args, DB> + sqlx::Type<DB>,
{
    builder
        .push_bind(archive_jid)
        .push(" AND origin_id = ")
        .push_bind(origin_id)
        .push(" AND message_type = ")
        .push_bind(message_type);
    if let Some(from_jid) = groupchat_from {
        builder.push(" AND from_jid = ").push_bind(from_jid);
    }
    if let Some(fingerprint) = origin_dedup_fingerprint {
        builder
            .push(" AND (origin_dedup_fingerprint = ")
            .push_bind(fingerprint)
            .push(" OR origin_dedup_fingerprint IS NULL)");
    }
    builder.push(" ORDER BY timestamp ASC, id ASC");
}

fn origin_dedup_fingerprint(message: &ArchivedMessage) -> String {
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
    // Hash the CONTENT-ONLY rich payload, not the full one bound to the
    // `rich_payload` column: the server-derived MUC identity fields
    // (`occupant_id`, `muc_sender`) carry per-session data
    // (`muc_sender.jid` is a fresh random resource each reconnect), so
    // including them would make every fresh-session origin-id retry
    // fingerprint differently and defeat dedup. `content_only()` clears
    // them, matching the in-memory `origin_id_dedup_match` check.
    let dedup_rich = message
        .rich
        .as_ref()
        .and_then(|rich| rich.dedup_content())
        .as_ref()
        .and_then(|rich| serde_json::to_string(rich).ok());
    update_hash_part(&mut hasher, "rich_payload", dedup_rich.as_deref());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to String never fails");
    }
    hex
}

fn origin_dedup_sender_scope(message: &ArchivedMessage) -> Option<String> {
    if !matches!(message.message_type, MessageType::Groupchat) || message.origin_id.is_none() {
        return None;
    }
    message
        .rich
        .as_ref()
        .and_then(|rich| rich.muc_sender.as_ref())
        .map(|sender| sender.jid.to_bare().to_string())
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
        // XEP-0424 §Tombstones: the occupant-id and real-JID item
        // identify the original sender and MUST NOT survive the
        // tombstone replacement.
        occupant_id: None,
        muc_sender: None,
    };
    let encoded = serde_json::to_string(&payload)
        .map_err(|error| MamStorageError::Serialization(error.to_string()))?;

    // XEP-0424 §Tombstones / XEP-0425 §Tombstones: drop the body
    // entirely on tombstone. With wire-fidelity body semantics, SQL NULL
    // means "no body element" and `''` remains distinct.
    let rows = match backend {
        MamDatabaseBackend::Sqlite(pool) => {
            let mut builder = QueryBuilder::<Sqlite>::new(
                "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, origin_dedup_sender_scope = NULL, origin_dedup_fingerprint = NULL, rich_payload = ",
            );
            builder
                .push_bind(encoded.as_str())
                .push(" WHERE id = ")
                .push_bind(archive_id);
            builder.build().execute(pool).await?.rows_affected()
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut builder = QueryBuilder::<Postgres>::new(
                "UPDATE mam_messages SET body = NULL, stanza_xml = NULL, thread_id = NULL, parent_thread_id = NULL, reply_to_id = NULL, reply_to_jid = NULL, origin_dedup_sender_scope = NULL, origin_dedup_fingerprint = NULL, rich_payload = ",
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
