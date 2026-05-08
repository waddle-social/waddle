use jid::BareJid;
use sqlx::{Postgres, QueryBuilder, Sqlite};
use tracing::debug;
use uuid::Uuid;
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload};

use crate::mam::storage::MamStorageError;

use super::decode::{encode_nickname_generation, encode_rich_payload};
use super::MamDatabaseBackend;

pub(super) async fn store_message(
    backend: &MamDatabaseBackend,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<String, MamStorageError> {
    let archive_id = if message.id.is_empty() {
        Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };
    // Typed-payloads boundary: convert the closed `MessageType`
    // enum to its canonical wire literal exactly once, here at
    // the SQL bind site.
    let message_type = waddle_xmpp_core::mam::message_type_wire_str(&message.message_type);
    let rich_payload = encode_rich_payload(message)?;
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
                "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
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
                    );
            });
            query.build().execute(pool).await?;
        }
        MamDatabaseBackend::Postgres(pool) => {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, thread_id, reply_to_id, reply_to_jid, origin_id, message_type, stanza_xml, rich_payload, nickname_generation, parent_thread_id) ",
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
                    );
            });
            query.build().execute(pool).await?;
        }
    }

    debug!(archive_id = %archive_id, "Message stored in MAM archive");
    Ok(archive_id)
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
