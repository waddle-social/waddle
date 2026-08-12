use std::num::TryFromIntError;

use jid::BareJid;
use sqlx::PgConnection;
use thiserror::Error;
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::StanzaId;

use crate::mam::storage::StoreOutcome;

use super::write::{
    find_existing_origin_id_match_postgres_on_connection, insert_postgres_message_on_connection,
    origin_dedup_fingerprint, origin_dedup_sender_scope, PostgresInsertConflict,
    PostgresMessageInsert,
};

/// Outcome of a MAM archive write performed within a caller-owned Postgres transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MamTxStoreOutcome {
    /// A new archive row was inserted.
    Inserted(StanzaId),
    /// A live archive row already matched the retry's origin-id.
    Existing(StanzaId),
    /// A retracted archive row matched the retry's origin-id.
    TombstoneHit(StanzaId),
}

/// Typed encoding failures raised before an archive row reaches Postgres.
#[derive(Debug, Error)]
pub enum MamTxEncodingError {
    #[error("failed to encode MAM rich payload")]
    RichPayload(#[source] serde_json::Error),
    #[error("nickname generation does not fit the Postgres column")]
    NicknameGeneration(#[source] TryFromIntError),
}

/// Errors from a caller-owned Postgres MAM archive write.
#[derive(Debug, Error)]
pub enum MamTxStoreError {
    #[error("MAM archive database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("MAM archive encoding error: {0}")]
    Encoding(#[from] MamTxEncodingError),
    #[error("MAM archive id conflicted without a matching origin-id row: {archive_id}")]
    Conflict { archive_id: String },
}

/// Store an archived message on a caller-owned Postgres connection.
///
/// The caller owns transaction boundaries. This function never begins,
/// commits, or rolls back a transaction.
pub async fn store_archived_message_on_connection(
    conn: &mut PgConnection,
    archive_jid: &BareJid,
    message: &ArchivedMessage,
) -> Result<MamTxStoreOutcome, MamTxStoreError> {
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
    let origin_dedup_fingerprint = message
        .origin_id
        .as_ref()
        .filter(|_| !super::super::origin_dedup::groupchat_subject_is_retry_dedup_exempt(message))
        .map(|_| origin_dedup_fingerprint(message));
    let origin_dedup_sender_scope = origin_dedup_sender_scope(message);
    let origin_dedup_sender_scope_bind = origin_dedup_sender_scope.as_ref().map(BareJid::to_string);
    let archive_id = if message.id.is_empty() {
        uuid::Uuid::now_v7().to_string()
    } else {
        message.id.clone()
    };

    if let Some(outcome) = find_existing_origin_id_match_postgres_on_connection(
        conn,
        archive_jid,
        message,
        origin_dedup_fingerprint.as_deref(),
    )
    .await?
    {
        return Ok(tx_outcome(outcome, archive_jid));
    }

    if insert_postgres_message_on_connection(
        conn,
        PostgresMessageInsert {
            archive_id: &archive_id,
            archive_jid,
            message,
            rich_payload: rich_payload.as_deref(),
            nickname_generation,
            origin_dedup_sender_scope: origin_dedup_sender_scope_bind.as_deref(),
            origin_dedup_fingerprint: origin_dedup_fingerprint.as_deref(),
        },
        PostgresInsertConflict::DoNothing,
    )
    .await?
    .is_some()
    {
        return Ok(MamTxStoreOutcome::Inserted(stanza_id(
            archive_id,
            archive_jid,
        )));
    }

    if let Some(outcome) = find_existing_origin_id_match_postgres_on_connection(
        conn,
        archive_jid,
        message,
        origin_dedup_fingerprint.as_deref(),
    )
    .await?
    {
        return Ok(tx_outcome(outcome, archive_jid));
    }

    Err(MamTxStoreError::Conflict { archive_id })
}

fn tx_outcome(outcome: StoreOutcome, archive_jid: &BareJid) -> MamTxStoreOutcome {
    match outcome {
        StoreOutcome::Stored(archive_id) | StoreOutcome::Deduplicated(archive_id) => {
            MamTxStoreOutcome::Existing(stanza_id(archive_id, archive_jid))
        }
        StoreOutcome::TombstoneHit(archive_id) => {
            MamTxStoreOutcome::TombstoneHit(stanza_id(archive_id, archive_jid))
        }
    }
}

fn stanza_id(archive_id: String, archive_jid: &BareJid) -> StanzaId {
    StanzaId::new(archive_id, jid::Jid::from(archive_jid.clone()))
}

#[cfg(test)]
mod tests {
    use std::env;

    use chrono::Utc;
    use sqlx::postgres::PgPoolOptions;
    use tokio::sync::oneshot;
    use waddle_xmpp_core::mam::{
        ArchivedMessage, ArchivedMucSender, ArchivedRichMessage, ArchivedTombstone,
    };
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;
    use xmpp_parsers::message::MessageType;

    use crate::mam::storage::{MamStorage, SqlxMamStorage};

    use super::{store_archived_message_on_connection, MamTxStoreError, MamTxStoreOutcome};

    fn postgres_url() -> Option<String> {
        env::var("WADDLE_TEST_POSTGRES_URL").ok()
    }

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

    async fn count_rows(pool: &sqlx::PgPool, archive: &jid::BareJid) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM mam_messages WHERE room_jid = $1")
            .bind(archive.to_string())
            .fetch_one(pool)
            .await
            .expect("count archive rows")
    }

    #[tokio::test]
    async fn postgres_tx_write_keeps_insert_commit_dedup_conflict_and_tombstone_semantics() {
        let Some(url) = postgres_url() else {
            eprintln!("skipping Postgres MAM tx-write test: WADDLE_TEST_POSTGRES_URL is unset");
            return;
        };
        let storage = SqlxMamStorage::open(&url)
            .await
            .expect("initialize MAM schema");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("connect Postgres test pool");
        let archive = unique_archive();
        let inserted = fixture(&archive, "rolled-back-id", Some("rolled-back-origin"));

        let mut rollback_tx = pool.begin().await.expect("begin rollback transaction");
        let rollback_outcome =
            store_archived_message_on_connection(&mut rollback_tx, &archive, &inserted)
                .await
                .expect("insert inside transaction");
        assert!(matches!(
            rollback_outcome,
            MamTxStoreOutcome::Inserted(ref stanza_id) if stanza_id.id == "rolled-back-id"
        ));
        rollback_tx
            .rollback()
            .await
            .expect("roll back archive insert");
        assert_eq!(count_rows(&pool, &archive).await, 0);

        let committed = fixture(&archive, "committed-id", Some("committed-origin"));
        let mut commit_tx = pool.begin().await.expect("begin commit transaction");
        assert!(matches!(
            store_archived_message_on_connection(&mut commit_tx, &archive, &committed)
                .await
                .expect("insert before commit"),
            MamTxStoreOutcome::Inserted(_)
        ));
        commit_tx.commit().await.expect("commit archive insert");
        assert_eq!(count_rows(&pool, &archive).await, 1);

        let mut dedup_tx = pool.begin().await.expect("begin dedup transaction");
        let retry = fixture(&archive, "retry-id", Some("committed-origin"));
        assert!(matches!(
            store_archived_message_on_connection(&mut dedup_tx, &archive, &retry)
                .await
                .expect("deduplicate retry"),
            MamTxStoreOutcome::Existing(ref stanza_id) if stanza_id.id == "committed-id"
        ));
        dedup_tx.commit().await.expect("commit dedup transaction");

        let conflicting = fixture(&archive, "conflict-id", None);
        let mut conflict_seed_tx = pool.begin().await.expect("begin conflict seed transaction");
        store_archived_message_on_connection(&mut conflict_seed_tx, &archive, &conflicting)
            .await
            .expect("seed primary-key conflict");
        conflict_seed_tx
            .commit()
            .await
            .expect("commit conflict seed");
        let mut conflict_tx = pool.begin().await.expect("begin conflict transaction");
        let conflict = fixture(&archive, "conflict-id", Some("different-origin"));
        assert!(matches!(
            store_archived_message_on_connection(&mut conflict_tx, &archive, &conflict).await,
            Err(MamTxStoreError::Conflict { ref archive_id }) if archive_id == "conflict-id"
        ));
        conflict_tx
            .rollback()
            .await
            .expect("roll back conflict transaction");

        let tombstone = ArchivedTombstone {
            retraction_id: None,
            stamp: Utc::now(),
            moderation: None,
            sender_scope: None,
        };
        assert!(storage
            .replace_with_tombstone("committed-id", tombstone)
            .await
            .expect("replace committed row with tombstone"));
        let mut tombstone_tx = pool
            .begin()
            .await
            .expect("begin tombstone retry transaction");
        let tombstone_retry = fixture(&archive, "tombstone-retry", Some("committed-origin"));
        assert!(matches!(
            store_archived_message_on_connection(&mut tombstone_tx, &archive, &tombstone_retry)
                .await
                .expect("recognize tombstone retry"),
            MamTxStoreOutcome::TombstoneHit(ref stanza_id) if stanza_id.id == "committed-id"
        ));
        tombstone_tx
            .commit()
            .await
            .expect("commit tombstone retry transaction");
    }

    #[tokio::test]
    async fn postgres_tx_write_dedup_race_leaves_one_row_and_no_poisoned_transaction() {
        let Some(url) = postgres_url() else {
            eprintln!(
                "skipping Postgres MAM tx-write race test: WADDLE_TEST_POSTGRES_URL is unset"
            );
            return;
        };
        SqlxMamStorage::open(&url)
            .await
            .expect("initialize MAM schema");
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect(&url)
            .await
            .expect("connect Postgres test pool");
        let archive = unique_archive();
        let first = fixture(&archive, "race-first", Some("race-origin"));
        let second = fixture(&archive, "race-second", Some("race-origin"));

        let (first_opened_tx, first_opened_rx) = oneshot::channel();
        let (second_opened_tx, second_opened_rx) = oneshot::channel();
        let (start_first_tx, start_first_rx) = oneshot::channel();
        let (first_inserted_tx, first_inserted_rx) = oneshot::channel();
        let (start_second_tx, start_second_rx) = oneshot::channel();
        let (commit_first_tx, commit_first_rx) = oneshot::channel();

        let first_pool = pool.clone();
        let first_archive = archive.clone();
        let first_task = tokio::spawn(async move {
            let mut tx = first_pool.begin().await.expect("begin first transaction");
            first_opened_tx
                .send(())
                .expect("signal first transaction open");
            start_first_rx.await.expect("start first transaction write");
            let outcome = store_archived_message_on_connection(&mut tx, &first_archive, &first)
                .await
                .expect("first transaction insert");
            first_inserted_tx.send(()).expect("signal first insert");
            commit_first_rx.await.expect("commit first transaction");
            tx.commit().await.expect("commit first transaction");
            outcome
        });
        let second_pool = pool.clone();
        let second_archive = archive.clone();
        let second_task = tokio::spawn(async move {
            let mut tx = second_pool.begin().await.expect("begin second transaction");
            second_opened_tx
                .send(())
                .expect("signal second transaction open");
            start_second_rx
                .await
                .expect("start second transaction write");
            let outcome = store_archived_message_on_connection(&mut tx, &second_archive, &second)
                .await
                .expect("second transaction resolves dedup");
            tx.commit().await.expect("commit second transaction");
            outcome
        });

        first_opened_rx.await.expect("wait for first transaction");
        second_opened_rx.await.expect("wait for second transaction");
        start_first_tx.send(()).expect("start first write");
        first_inserted_rx.await.expect("wait for first insert");
        start_second_tx.send(()).expect("start second write");
        commit_first_tx.send(()).expect("commit first write");

        assert!(matches!(
            first_task.await.expect("first task completed"),
            MamTxStoreOutcome::Inserted(_)
        ));
        assert!(matches!(
            second_task.await.expect("second task completed"),
            MamTxStoreOutcome::Existing(ref stanza_id) if stanza_id.id == "race-first"
        ));
        assert_eq!(count_rows(&pool, &archive).await, 1);
    }
}
