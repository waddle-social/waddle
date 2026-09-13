use super::super::*;
use crate::{
    ingress::test_support::IngressFixture,
    ingress_uow::{
        CanonicalMessageRepository, ConfiguredPluginGrants, EffectIntentRepository,
        EffectReceiptRepository, ExtensionGrantRepository,
    },
    notification_outbox::NotificationOutboxStore,
    pending_delivery::DatabasePendingDeliveryStorage,
};
use std::time::Duration;
use waddle_xmpp::pending_delivery::QuotaPolicy;

pub(super) async fn adapter(f: &IngressFixture) -> ExtensionHostAdapter {
    crate::pubsub::DatabasePubSubStorage::open(Some(f.db.database_url()))
        .await
        .expect("notification policy schema");
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(f.db.driver(), f.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("shared pool");
    let mut state = crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool), Arc::new(f.authority().await),
    ).await;
    let protocol = &mut Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol;
    protocol.pending_delivery_storage = Arc::new(
        DatabasePendingDeliveryStorage::from_database(f.db.clone(), QuotaPolicy::Unlimited)
            .await
            .expect("pending storage"),
    );
    protocol.notification_settings_projection = Arc::new(
        crate::notification_settings_projection::NotificationSettingsProjectionStore::new(
            f.db.clone(),
        ),
    );
    NotificationOutboxStore::new(f.db.clone())
        .await
        .expect("outbox schema");
    f.execute("INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES ('juliet@example.com', 'juliet', 'juliet', ?, ?)", crate::db_params![chrono::Utc::now().to_rfc3339(), chrono::Utc::now().to_rfc3339()]).await;
    f.execute("INSERT INTO roster_items (user_jid, contact_jid, subscription, approved, groups, updated_at) VALUES ('romeo@example.com', 'juliet@example.com', 'both', FALSE, '[]', ?)", crate::db_params![chrono::Utc::now().to_rfc3339()]).await;
    let mut tx = f.uow.begin().await.expect("grant tx");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: plugin(),
            can_send: true,
            provider_rooms: vec![],
        }],
    )
    .await
    .expect("configured send grant");
    tx.commit().await.expect("grant commit");
    ExtensionHostAdapter::new(state)
}

pub(super) fn plugin() -> PluginId {
    PluginId::new("direct-test").expect("plugin")
}

pub(super) fn invocation() -> ExtensionInvocation {
    ExtensionInvocation {
        session: Some(Session::new("romeo@example.com", "romeo", "romeo")),
        actor_jid: "romeo@example.com/extension-host".parse().expect("sender"),
        plugin_id: plugin(),
        source_room: None,
        kind: InvocationKind::MessageHook,
        provider_room_grants: vec![],
    }
}

pub(super) fn request(origin: &str) -> HostSendMessage {
    HostSendMessage {
        target: HostMessageTarget::Direct("juliet@example.com".parse().expect("target")),
        stanza_id: StanzaId::new(origin).expect("origin"),
        body: "extension direct body".into(),
        thread_id: None,
        reply_to: None,
        markup: vec![],
        extensions: None,
    }
}

async fn direct_offline_replay(f: IngressFixture) {
    let adapter = adapter(&f).await;
    let origin = "extension-direct-replay";
    assert_eq!(
        adapter
            .send_message(&invocation(), request(origin))
            .await
            .expect("accepted")
            .as_str(),
        origin
    );
    assert_eq!(
        f.count("ingress_messages").await,
        1,
        "real adapter must commit ingress"
    );
    assert_eq!(f.count("pending_delivery").await, 1);
    assert_eq!(f.count("notification_candidates").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    let receipts = f.count("ingress_effect_receipts").await;
    let key = waddle_xmpp::ingress::MessageKey::from_storage(
        f.optional_text("SELECT CAST(message_key AS TEXT) FROM ingress_messages")
            .await
            .expect("message key")
            .parse()
            .expect("uuid"),
    );
    let mut tx = f.uow.begin().await.expect("inspect canonical authority");
    let envelope = CanonicalMessageRepository::load_envelope(&mut tx, key)
        .await
        .expect("load envelope")
        .expect("canonical message");
    assert_eq!(
        envelope.message().from,
        Some("romeo@example.com/extension-host".parse().expect("sender"))
    );
    assert_eq!(
        envelope.message().id.as_ref().map(|id| id.0.as_str()),
        Some(origin)
    );
    assert_eq!(
        waddle_xmpp_core::xep0359::extract_origin_id(envelope.message())
            .expect("origin id")
            .as_str(),
        origin
    );
    let intents = EffectIntentRepository::load(&mut tx, key)
        .await
        .expect("recorded intents");
    use waddle_xmpp::ingress::{
        IngressEffectIntent, NotificationActivityMutation, NotificationCandidateOutcome,
    };
    assert_eq!(
        intents
            .iter()
            .filter(|intent| matches!(intent, IngressEffectIntent::PendingDelivery { .. }))
            .count(),
        1
    );
    assert_eq!(
        intents
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::NotificationActivityPreview {
                    mutation: NotificationActivityMutation::NotificationCandidate {
                        outcome: NotificationCandidateOutcome::Inserted,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        intents
            .iter()
            .filter(|intent| matches!(
                intent,
                IngressEffectIntent::NotificationActivityPreview {
                    mutation: NotificationActivityMutation::OfflineDelivery { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(!intents
        .iter()
        .any(|intent| intent.kind() == waddle_xmpp::ingress::IngressEffectKind::RouteDirect));
    assert!(EffectReceiptRepository::receipts_complete(&mut tx, key)
        .await
        .expect("complete receipts"));
    tx.commit().await.expect("inspection commit");
    let archive_count = f.count("mam_messages").await;
    assert_eq!(archive_count, 2, "sender and recipient archives");
    let sender_archive = f
        .optional_text("SELECT id FROM mam_messages WHERE room_jid = 'romeo@example.com'")
        .await
        .expect("sender archive id");
    let recipient_archive = f
        .optional_text("SELECT id FROM mam_messages WHERE room_jid = 'juliet@example.com'")
        .await
        .expect("recipient archive id");
    adapter
        .send_message(&invocation(), request(origin))
        .await
        .expect("replay accepted");
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("pending_delivery").await, 1);
    assert_eq!(f.count("notification_candidates").await, 1);
    assert_eq!(f.count("mam_messages").await, archive_count);
    assert_eq!(
        f.optional_text("SELECT id FROM mam_messages WHERE room_jid = 'romeo@example.com'")
            .await,
        Some(sender_archive)
    );
    assert_eq!(
        f.optional_text("SELECT id FROM mam_messages WHERE room_jid = 'juliet@example.com'")
            .await,
        Some(recipient_archive)
    );
    assert_eq!(f.count("ingress_effect_receipts").await, receipts);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_offline_replay_sqlite() {
    direct_offline_replay(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_direct_offline_replay_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_direct").await {
        direct_offline_replay(f).await;
    }
}

async fn revoked_between_sends(f: IngressFixture) {
    let adapter = adapter(&f).await;
    adapter
        .send_message(&invocation(), request("before-revoke"))
        .await
        .expect("first send");
    let mut tx = f.uow.begin().await.expect("revocation tx");
    assert_eq!(
        ExtensionGrantRepository::revoke_plugin(&mut tx, &plugin())
            .await
            .expect("revoke"),
        1
    );
    tx.commit().await.expect("revoke commit");
    assert!(matches!(
        adapter
            .send_message(&invocation(), request("after-revoke"))
            .await,
        Err(ExtensionHostAdapterError::NotAuthorized)
    ));
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("pending_delivery").await, 1);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_revocation_sqlite() {
    revoked_between_sends(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_direct_revocation_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_direct_revoked").await {
        revoked_between_sends(f).await;
    }
}

async fn blocked_requester(f: IngressFixture) {
    let mut adapter = adapter(&f).await;
    let blocking = waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new();
    blocking.set_blocklist(
        "juliet@example.com".parse().expect("recipient"),
        vec!["romeo@example.com".parse().expect("sender")],
    );
    Arc::get_mut(&mut adapter.state)
        .expect("unique state")
        .deps
        .protocol
        .blocking_storage = Arc::new(blocking);
    let result = adapter
        .send_message(&invocation(), request("blocked-requester"))
        .await;
    let Err(ExtensionHostAdapterError::Rejected(error)) = result else {
        panic!("blocked recipient must reject at host transport: {result:?}")
    };
    assert_eq!(
        error.defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 11").await, 1);
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_direct_blocking_sqlite() {
    blocked_requester(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn extension_direct_blocking_postgres() {
    if let Some(f) = IngressFixture::postgres("extension_direct_blocking").await {
        blocked_requester(f).await;
    }
}
