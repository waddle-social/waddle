//! Ordinary offline delivery requires the ingress transaction arm.
use super::super::*;
use crate::{
    notification_outbox::{direct_candidate_from_envelope, NotificationOutboxStore},
    pending_delivery::DatabasePendingDeliveryStorage,
    server::routes::websocket::tests as socket_tests,
};
use std::sync::Arc;
use waddle_xmpp::pending_delivery::{
    storage::PendingDeliveryStorage, PendingPayload, PendingRow, PendingRowId, QuotaPolicy,
};

fn offline_row() -> (PendingRow, xmpp_parsers::message::Message) {
    let recipient: jid::BareJid = format!("immediate-{}@example.com", uuid::Uuid::new_v4())
        .parse()
        .expect("recipient");
    let archive = waddle_xmpp_core::xep0359::StanzaId::new(
        uuid::Uuid::new_v4().to_string(),
        recipient.clone().into(),
    );
    let mut message = xmpp_parsers::message::Message::new(Some(recipient.clone().into()));
    message.from = Some("alice@example.com/phone".parse().expect("sender"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang(String::new()),
        "extension offline message".into(),
    );
    (
        PendingRow {
            id: PendingRowId::fresh(),
            recipient,
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(archive),
            flushed_in_session: None,
            outbound_sequence: None,
        },
        message,
    )
}

async fn immediate_offline_delivery(database_url: Option<&str>) {
    let database_storage =
        DatabasePendingDeliveryStorage::open(database_url, QuotaPolicy::Unlimited)
            .await
            .expect("pending storage");
    let database = database_storage.database();
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(database_storage);
    let mut state = socket_tests::create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("unique test state")
        .deps
        .protocol
        .notification_outbox = Arc::new(
        NotificationOutboxStore::new(database.clone())
            .await
            .expect("notification outbox on tested dialect"),
    );
    let deps = Deps {
        pending_delivery_storage: Some(&storage),
        web_socket_state: Some(&state),
        ..Deps::registry_only(&state.deps.protocol.connection_registry)
    };
    let (row, message) = offline_row();
    let PendingPayload::Archived(archive) = &row.payload else {
        panic!("archived fixture");
    };
    let candidate = direct_candidate_from_envelope(
        &message,
        &row.recipient,
        message.from.as_ref().expect("sender"),
        archive,
    )
    .expect("prepared candidate");
    let outcome = execute(
        ExternalDeliveryEffect::QueueOfflineDelivery {
            row: row.clone(),
            original_message: Box::new(message.clone()),
            prepared_notification:
                super::super::super::delivery::PreparedOfflineNotification::Prepared(Box::new(
                    candidate.clone(),
                )),
        },
        &deps,
    )
    .await;
    assert!(matches!(outcome, EffectOutcome::Unavailable), "{outcome:?}");
    assert!(storage
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    assert_eq!(
        state
            .deps
            .protocol
            .notification_outbox
            .count_all_candidates()
            .await
            .expect("candidate count"),
        0,
    );
}

async fn immediate_offline_quota(database_url: Option<&str>) {
    let storage: Arc<dyn PendingDeliveryStorage> = Arc::new(
        DatabasePendingDeliveryStorage::open(database_url, QuotaPolicy::CountCap { max_rows: 0 })
            .await
            .expect("pending storage"),
    );
    let state = socket_tests::create_test_websocket_state().await;
    let sender = "alice@example.com/phone".parse().expect("sender");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &sender, tx).await;
    let deps = Deps {
        pending_delivery_storage: Some(&storage),
        ..Deps::registry_only(&state.deps.protocol.connection_registry)
    };
    let (row, message) = offline_row();
    let outcome = execute(
        ExternalDeliveryEffect::QueueOfflineDelivery {
            row: row.clone(),
            original_message: Box::new(message),
            prepared_notification:
                super::super::super::delivery::PreparedOfflineNotification::Suppressed,
        },
        &deps,
    )
    .await;
    assert!(matches!(outcome, EffectOutcome::Unavailable));
    assert!(storage
        .list(&row.recipient)
        .await
        .expect("pending rows")
        .is_empty());
    assert!(rx.try_recv().is_err(), "uncommitted work cannot bounce");
}

async fn postgres_fixture() -> Option<(crate::db::Database, String, String)> {
    let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("WADDLE_TEST_POSTGRES_URL unset: skipping Postgres immediate offline test");
        return None;
    };
    let db = crate::db::Database::from_config(
        "immediate_offline_test_schema",
        &crate::db::DatabaseConfig::new(crate::db::DatabaseDriver::Postgres, &database_url),
    )
    .await
    .expect("Postgres schema connection");
    let schema = format!("immediate_offline_{}", uuid::Uuid::new_v4().simple());
    db.guard()
        .await
        .expect("schema connection")
        .execute(&format!("CREATE SCHEMA {schema}"), ())
        .await
        .expect("create isolated schema");
    let mut url = url::Url::parse(&database_url).expect("Postgres URL");
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    Some((db, schema, url.to_string()))
}

async fn drop_postgres_fixture(db: crate::db::Database, schema: String) {
    db.guard()
        .await
        .expect("schema cleanup connection")
        .execute(&format!("DROP SCHEMA {schema} CASCADE"), ())
        .await
        .expect("drop isolated schema");
}

#[tokio::test]
async fn sqlite_immediate_offline_delivery_requires_ingress() {
    immediate_offline_delivery(None).await;
}

#[tokio::test]
async fn postgres_immediate_offline_delivery_requires_ingress() {
    let Some((db, schema, url)) = postgres_fixture().await else {
        return;
    };
    immediate_offline_delivery(Some(&url)).await;
    drop_postgres_fixture(db, schema).await;
}

#[tokio::test]
async fn sqlite_immediate_offline_quota_requires_ingress() {
    immediate_offline_quota(None).await;
}

#[tokio::test]
async fn postgres_immediate_offline_quota_requires_ingress() {
    let Some((db, schema, url)) = postgres_fixture().await else {
        return;
    };
    immediate_offline_quota(Some(&url)).await;
    drop_postgres_fixture(db, schema).await;
}
