use jid::BareJid;
use sqlx::postgres::PgPoolOptions;
use waddle_xmpp_core::mam::ArchivedMessage;

use super::SqlxMamStorage;
use crate::mam::storage::MamStorageError;
use crate::muc::RoomClaimFenceContext;
use crate::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

#[tokio::test]
async fn postgres_fenced_store_rejects_claim_for_different_archive_before_write() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping fenced MAM write test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("pool");
    let mut tx = pool.begin().await.expect("transaction");

    let mam_schema = format!("mam_fenced_{}", uuid::Uuid::now_v7().simple());
    let claim_schema = format!("claims_fenced_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE SCHEMA \"{mam_schema}\""))
        .execute(&mut *tx)
        .await
        .expect("MAM schema");
    sqlx::query(&format!("CREATE SCHEMA \"{claim_schema}\""))
        .execute(&mut *tx)
        .await
        .expect("claim schema");

    let messages = super::postgres_relation(Some(&mam_schema), "mam_messages");
    let sequences = super::postgres_relation(Some(&mam_schema), "mam_archive_sequences");
    let claims = super::postgres_relation(Some(&claim_schema), "clustering_claims");
    sqlx::query(&format!(
        "CREATE TABLE {messages} (id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, timestamp TIMESTAMPTZ NOT NULL, from_jid TEXT NOT NULL, to_jid TEXT NOT NULL, body TEXT, stanza_id TEXT, thread_id TEXT, reply_to_id TEXT, reply_to_jid TEXT, origin_id TEXT, message_type TEXT NOT NULL, stanza_xml TEXT, rich_payload TEXT, nickname_generation BIGINT, parent_thread_id TEXT, archive_seq BIGINT NOT NULL)"
    ))
    .execute(&mut *tx)
    .await
    .expect("MAM message table");
    sqlx::query(&format!(
        "CREATE TABLE {sequences} (archive_jid TEXT PRIMARY KEY, next_seq BIGINT NOT NULL)"
    ))
    .execute(&mut *tx)
    .await
    .expect("MAM sequence table");
    sqlx::query(&format!(
        "CREATE TABLE {claims} (entity TEXT PRIMARY KEY, entity_type TEXT NOT NULL, node_id TEXT NOT NULL, node_epoch TEXT NOT NULL, claim_epoch BIGINT NOT NULL)"
    ))
    .execute(&mut *tx)
    .await
    .expect("claim table");

    let archive_a: BareJid = "room-a@rooms.example".parse().expect("room A");
    let archive_b: BareJid = "room-b@rooms.example".parse().expect("room B");
    let owner = NodeIdentity::new("node-a", "epoch-a");
    let fence = RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, archive_a.to_string()),
        owner.clone(),
        ClaimEpoch(23),
    );
    let entity_key = format!("{}:{}", EntityType::RoomActor.as_db_str(), archive_a);
    sqlx::query(&format!(
        "INSERT INTO {claims} (entity, entity_type, node_id, node_epoch, claim_epoch) VALUES ($1, $2, $3, $4, $5)"
    ))
    .bind(&entity_key)
    .bind(EntityType::RoomActor.as_db_str())
    .bind(&owner.node_id)
    .bind(&owner.node_epoch)
    .bind(fence.epoch.0)
    .execute(&mut *tx)
    .await
    .expect("valid room A claim");

    let message = ArchivedMessage {
        id: "cross-room-write".to_owned(),
        body: Some("must not be archived in room B".to_owned()),
        ..ArchivedMessage::for_test(
            "alice@example.com/device".parse().expect("sender"),
            archive_b.clone().into(),
        )
    };
    let outcome = SqlxMamStorage::store_message_fenced_on_postgres_connection_in_schema(
        &mut tx,
        &mam_schema,
        &claim_schema,
        &archive_b,
        &message,
        &fence,
    )
    .await;
    assert!(matches!(
        outcome,
        Err(MamStorageError::NotOwner { entity }) if entity == fence.entity
    ));

    let archived_in_b: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {messages} WHERE room_jid = $1"
    ))
    .bind(archive_b.as_str())
    .fetch_one(&mut *tx)
    .await
    .expect("room B archive count");
    let sequence_for_b: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {sequences} WHERE archive_jid = $1"
    ))
    .bind(archive_b.as_str())
    .fetch_one(&mut *tx)
    .await
    .expect("room B sequence count");
    assert_eq!(archived_in_b, 0);
    assert_eq!(sequence_for_b, 0);

    tx.rollback().await.expect("rollback schema and fixtures");
}
