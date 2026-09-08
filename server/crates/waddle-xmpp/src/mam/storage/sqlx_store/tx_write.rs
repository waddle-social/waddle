use super::write::{
    find_origin_tombstone_postgres, find_origin_tombstone_sqlite,
    insert_postgres_message_on_connection, insert_sqlite_message_on_connection, InsertConflict,
    MessageInsert,
};
use chrono::{DateTime, Utc};
use jid::BareJid;
use sqlx::{PgConnection, SqliteConnection};
use std::num::TryFromIntError;
use thiserror::Error;
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichMessage};
use waddle_xmpp_core::xep0359::StanzaId;

/// Canonical ingress authority's expectation for this archive projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveExpectation {
    Fresh,
    Existing {
        stanza_id: StanzaId,
        archived_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MamTxStoreOutcome {
    Inserted(StanzaId),
    Existing(StanzaId),
    Repaired(StanzaId),
    TombstoneHit(StanzaId),
    /// Reserved for bounded archive retention; current MAM retention is unbounded.
    Expired(StanzaId),
}

#[derive(Debug, Error)]
pub enum MamTxEncodingError {
    #[error("failed to encode MAM rich payload")]
    RichPayload(#[source] serde_json::Error),
    #[error("nickname generation does not fit the archive column")]
    NicknameGeneration(#[source] TryFromIntError),
}

#[derive(Debug, Error)]
pub enum MamTxStoreError {
    #[error("MAM archive database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("MAM archive encoding error: {0}")]
    Encoding(#[from] MamTxEncodingError),
    #[error("MAM archive id conflicts with canonical identity: {}", stanza_id.id)]
    Conflict { stanza_id: StanzaId },
}

fn expected_message(
    archive: &BareJid,
    message: &ArchivedMessage,
    expectation: &ArchiveExpectation,
) -> Result<(ArchivedMessage, StanzaId), MamTxStoreError> {
    let mut message = message.clone();
    let id = match expectation {
        ArchiveExpectation::Fresh => StanzaId::new(
            if message.id.is_empty() {
                uuid::Uuid::now_v7().to_string()
            } else {
                message.id.clone()
            },
            archive.clone().into(),
        ),
        ArchiveExpectation::Existing {
            stanza_id,
            archived_at,
        } => {
            if stanza_id.by != *archive {
                return Err(MamTxStoreError::Conflict {
                    stanza_id: stanza_id.clone(),
                });
            }
            message.timestamp = *archived_at;
            stanza_id.clone()
        }
    };
    message.id = id.id.clone();
    Ok((message, id))
}

/// Store using a caller-owned connection; transaction boundaries remain with the caller.
/// MAM currently has unbounded retention, so absent recorded identities are repaired.
pub async fn store_archived_message_on_connection(
    conn: &mut PgConnection,
    archive: &BareJid,
    message: &ArchivedMessage,
    expectation: ArchiveExpectation,
) -> Result<MamTxStoreOutcome, MamTxStoreError> {
    let (message, stanza_id) = expected_message(archive, message, &expectation)?;
    let existing: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT room_jid, rich_payload FROM mam_messages WHERE id = $1")
            .bind(&stanza_id.id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some((recorded_archive, payload)) = existing {
        if matches!(expectation, ArchiveExpectation::Fresh)
            || recorded_archive != archive.to_string()
        {
            return Err(MamTxStoreError::Conflict { stanza_id });
        }
        let rich: Option<ArchivedRichMessage> = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(MamTxEncodingError::RichPayload)?;
        return Ok(
            if rich
                .as_ref()
                .is_some_and(ArchivedRichMessage::is_tombstoned)
            {
                MamTxStoreOutcome::TombstoneHit(stanza_id)
            } else {
                MamTxStoreOutcome::Existing(stanza_id)
            },
        );
    }
    if let Some(id) = find_origin_tombstone_postgres(conn, archive, &message).await? {
        return Ok(MamTxStoreOutcome::TombstoneHit(StanzaId::new(
            id,
            archive.clone().into(),
        )));
    }
    let rich_payload = message
        .rich
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(MamTxEncodingError::RichPayload)?;
    let nickname_generation = message
        .nickname_generation
        .map(i64::try_from)
        .transpose()
        .map_err(MamTxEncodingError::NicknameGeneration)?;
    if insert_postgres_message_on_connection(
        conn,
        MessageInsert {
            archive_id: &message.id,
            archive_jid: archive,
            message: &message,
            rich_payload: rich_payload.as_deref(),
            nickname_generation,
        },
        InsertConflict::DoNothing,
    )
    .await?
    .is_none()
    {
        return Err(MamTxStoreError::Conflict { stanza_id });
    }
    Ok(match expectation {
        ArchiveExpectation::Fresh => MamTxStoreOutcome::Inserted(stanza_id),
        ArchiveExpectation::Existing { .. } => MamTxStoreOutcome::Repaired(stanza_id),
    })
}

/// Store using a caller-owned connection; transaction boundaries remain with the caller.
/// MAM currently has unbounded retention, so absent recorded identities are repaired.
pub async fn store_archived_message_on_sqlite_connection(
    conn: &mut SqliteConnection,
    archive: &BareJid,
    message: &ArchivedMessage,
    expectation: ArchiveExpectation,
) -> Result<MamTxStoreOutcome, MamTxStoreError> {
    let (message, stanza_id) = expected_message(archive, message, &expectation)?;
    let existing: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT room_jid, rich_payload FROM mam_messages WHERE id = $1")
            .bind(&stanza_id.id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some((recorded_archive, payload)) = existing {
        if matches!(expectation, ArchiveExpectation::Fresh)
            || recorded_archive != archive.to_string()
        {
            return Err(MamTxStoreError::Conflict { stanza_id });
        }
        let rich: Option<ArchivedRichMessage> = payload
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(MamTxEncodingError::RichPayload)?;
        return Ok(
            if rich
                .as_ref()
                .is_some_and(ArchivedRichMessage::is_tombstoned)
            {
                MamTxStoreOutcome::TombstoneHit(stanza_id)
            } else {
                MamTxStoreOutcome::Existing(stanza_id)
            },
        );
    }
    if let Some(id) = find_origin_tombstone_sqlite(conn, archive, &message).await? {
        return Ok(MamTxStoreOutcome::TombstoneHit(StanzaId::new(
            id,
            archive.clone().into(),
        )));
    }
    let rich_payload = message
        .rich
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(MamTxEncodingError::RichPayload)?;
    let nickname_generation = message
        .nickname_generation
        .map(i64::try_from)
        .transpose()
        .map_err(MamTxEncodingError::NicknameGeneration)?;
    if insert_sqlite_message_on_connection(
        conn,
        MessageInsert {
            archive_id: &message.id,
            archive_jid: archive,
            message: &message,
            rich_payload: rich_payload.as_deref(),
            nickname_generation,
        },
        InsertConflict::DoNothing,
    )
    .await?
    .is_none()
    {
        return Err(MamTxStoreError::Conflict { stanza_id });
    }
    Ok(match expectation {
        ArchiveExpectation::Fresh => MamTxStoreOutcome::Inserted(stanza_id),
        ArchiveExpectation::Existing { .. } => MamTxStoreOutcome::Repaired(stanza_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mam::storage::{MamStorage, SqlxMamStorage};
    use chrono::{Duration, Utc};
    use waddle_xmpp_core::mam::{
        ArchivedMessage, ArchivedMucSender, ArchivedRichMessage, ArchivedTombstone,
    };
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
    use xmpp_parsers::message::MessageType;
    fn fixture(archive: &jid::BareJid, id: &str, origin_id: Option<&str>) -> ArchivedMessage {
        ArchivedMessage {
            id: id.to_string(),
            body: Some("transactional MAM archive fixture".to_string()),
            origin_id: origin_id.map(OriginId::new),
            message_type: MessageType::Groupchat,
            rich: Some(ArchivedRichMessage {
                muc_sender: Some(ArchivedMucSender {
                    jid: "alice@example.com/session-a"
                        .parse()
                        .expect("valid test sender JID"),
                    affiliation: Affiliation::Member,
                    role: Role::Participant,
                }),
                ..ArchivedRichMessage::default()
            }),
            ..ArchivedMessage::for_test(
                format!("{archive}/alice")
                    .parse()
                    .expect("valid test occupant JID"),
                jid::Jid::from(archive.clone()),
            )
        }
    }

    fn unique_archive() -> jid::BareJid {
        format!("lane-a-{}@conference.example.com", uuid::Uuid::now_v7())
            .parse()
            .expect("valid test archive JID")
    }

    /// Archive ids are the GLOBAL `mam_messages` primary key and the test
    /// database persists across runs, so every fixture id must be minted
    /// per run — a constant id collides with a committed row from an
    /// earlier invocation and turns the insert into a `Conflict`.
    fn unique_id(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::now_v7())
    }

    #[tokio::test]
    async fn postgres_tx_write_preserves_canonical_identity_and_repairs_recorded_timestamp() {
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping Postgres MAM tx-write test: WADDLE_TEST_POSTGRES_URL is unset");
            return;
        };
        let storage = SqlxMamStorage::open(&url).await.expect("MAM schema");
        let pool = storage.postgres_pool().expect("Postgres pool");
        let archive = unique_archive();
        let mut message = fixture(&archive, &unique_id("canonical"), Some("origin"));
        let wire_id = unique_id("wire");
        message.stanza_id = Some(StanzaId::new(wire_id.clone(), archive.clone().into()));
        message.stanza_xml = None;
        let mut tx = pool.begin().await.expect("begin");
        assert!(matches!(
            store_archived_message_on_connection(
                &mut tx,
                &archive,
                &message,
                ArchiveExpectation::Fresh
            )
            .await
            .expect("insert"),
            MamTxStoreOutcome::Inserted(_)
        ));
        assert!(matches!(
            store_archived_message_on_connection(
                &mut tx,
                &archive,
                &message,
                ArchiveExpectation::Fresh
            )
            .await,
            Err(MamTxStoreError::Conflict { .. })
        ));
        tx.rollback().await.expect("rollback");
        let archived_at = Utc::now() - Duration::days(365);
        let id = StanzaId::new(message.id.clone(), archive.clone().into());
        let expected = ArchiveExpectation::Existing {
            stanza_id: id.clone(),
            archived_at,
        };
        let mut tx = pool.begin().await.expect("begin repair");
        assert_eq!(
            store_archived_message_on_connection(&mut tx, &archive, &message, expected.clone())
                .await
                .expect("repair"),
            MamTxStoreOutcome::Repaired(id.clone())
        );
        assert_eq!(
            store_archived_message_on_connection(&mut tx, &archive, &message, expected.clone())
                .await
                .expect("existing"),
            MamTxStoreOutcome::Existing(id.clone())
        );
        let another = fixture(&archive, &unique_id("same-origin"), Some("origin"));
        assert!(matches!(
            store_archived_message_on_connection(
                &mut tx,
                &archive,
                &another,
                ArchiveExpectation::Fresh
            )
            .await
            .expect("origin id does not deduplicate"),
            MamTxStoreOutcome::Inserted(_)
        ));
        tx.commit().await.expect("commit repair");
        let stored = storage
            .get_message(&message.id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(
            stored.timestamp.timestamp_millis(),
            archived_at.timestamp_millis()
        );
        assert_eq!(stored.stanza_id, message.stanza_id);
        assert_eq!(
            waddle_xmpp_core::mam::archived_inner_message(&stored).attr("id"),
            Some(wire_id.as_str())
        );
        storage
            .replace_with_tombstone(
                &message.id,
                ArchivedTombstone {
                    retraction_id: None,
                    stamp: Utc::now(),
                    moderation: None,
                    sender_scope: None,
                },
            )
            .await
            .expect("tombstone");
        let mut tx = pool.begin().await.expect("begin tombstone");
        assert_eq!(
            store_archived_message_on_connection(&mut tx, &archive, &message, expected)
                .await
                .expect("tombstone hit"),
            MamTxStoreOutcome::TombstoneHit(id)
        );
        assert!(matches!(
            store_archived_message_on_connection(
                &mut tx,
                &archive,
                &message,
                ArchiveExpectation::Fresh
            )
            .await,
            Err(MamTxStoreError::Conflict { .. })
        ));
        let retry = fixture(&archive, &unique_id("retry"), Some("origin"));
        assert!(matches!(
            store_archived_message_on_connection(
                &mut tx,
                &archive,
                &retry,
                ArchiveExpectation::Fresh
            )
            .await
            .expect("origin tombstone"),
            MamTxStoreOutcome::TombstoneHit(_)
        ));
        tx.rollback().await.expect("rollback");
    }

    #[tokio::test]
    async fn sqlite_tx_write_preserves_canonical_identity_and_repairs_recorded_timestamp() {
        let storage = SqlxMamStorage::open_in_memory().await.expect("MAM schema");
        let pool = storage.sqlite_pool().expect("SQLite pool");
        let archive = unique_archive();
        let mut message = fixture(&archive, &unique_id("canonical"), Some("origin"));
        let wire_id = unique_id("wire");
        message.stanza_id = Some(StanzaId::new(wire_id.clone(), archive.clone().into()));
        message.stanza_xml = None;
        let mut tx = pool.begin().await.expect("begin");
        assert!(matches!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &message,
                ArchiveExpectation::Fresh
            )
            .await
            .expect("insert"),
            MamTxStoreOutcome::Inserted(_)
        ));
        assert!(matches!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &message,
                ArchiveExpectation::Fresh
            )
            .await,
            Err(MamTxStoreError::Conflict { .. })
        ));
        tx.rollback().await.expect("rollback");
        let archived_at = Utc::now() - Duration::days(365);
        let id = StanzaId::new(message.id.clone(), archive.clone().into());
        let expected = ArchiveExpectation::Existing {
            stanza_id: id.clone(),
            archived_at,
        };
        let mut tx = pool.begin().await.expect("begin repair");
        assert_eq!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &message,
                expected.clone()
            )
            .await
            .expect("repair"),
            MamTxStoreOutcome::Repaired(id.clone())
        );
        assert_eq!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &message,
                expected.clone()
            )
            .await
            .expect("existing"),
            MamTxStoreOutcome::Existing(id.clone())
        );
        let another = fixture(&archive, &unique_id("same-origin"), Some("origin"));
        assert!(matches!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &another,
                ArchiveExpectation::Fresh
            )
            .await
            .expect("origin id does not deduplicate"),
            MamTxStoreOutcome::Inserted(_)
        ));
        tx.commit().await.expect("commit repair");
        let stored = storage
            .get_message(&message.id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(
            stored.timestamp.timestamp_millis(),
            archived_at.timestamp_millis()
        );
        assert_eq!(stored.stanza_id, message.stanza_id);
        assert_eq!(
            waddle_xmpp_core::mam::archived_inner_message(&stored).attr("id"),
            Some(wire_id.as_str())
        );
        storage
            .replace_with_tombstone(
                &message.id,
                ArchivedTombstone {
                    retraction_id: None,
                    stamp: Utc::now(),
                    moderation: None,
                    sender_scope: None,
                },
            )
            .await
            .expect("tombstone");
        let mut tx = pool.begin().await.expect("begin tombstone");
        assert_eq!(
            store_archived_message_on_sqlite_connection(&mut tx, &archive, &message, expected)
                .await
                .expect("tombstone hit"),
            MamTxStoreOutcome::TombstoneHit(id)
        );
        assert!(matches!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &message,
                ArchiveExpectation::Fresh
            )
            .await,
            Err(MamTxStoreError::Conflict { .. })
        ));
        let retry = fixture(&archive, &unique_id("retry"), Some("origin"));
        assert!(matches!(
            store_archived_message_on_sqlite_connection(
                &mut tx,
                &archive,
                &retry,
                ArchiveExpectation::Fresh
            )
            .await
            .expect("origin tombstone"),
            MamTxStoreOutcome::TombstoneHit(_)
        ));
        tx.rollback().await.expect("rollback");
    }
}
