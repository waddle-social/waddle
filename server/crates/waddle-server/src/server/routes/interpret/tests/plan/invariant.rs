use super::super::super::effects::delivery::PreparedOfflineNotification;
use super::super::super::effects::direct::ExternalDirectEffect;
use super::*;

#[tokio::test]
async fn planning_offline_dm_leaves_notification_stores_and_database_actor_untouched() {
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    crate::server::routes::websocket::tests::seed_local_account(&state, "alice").await;
    crate::server::routes::websocket::tests::seed_local_account(&state, "bob").await;
    let mam: Arc<dyn MamStorage> = Arc::new(poison::PoisonMam(InMemoryMamStorage::new()));
    let inbox: Arc<dyn InboxStorage> = Arc::new(poison::PoisonInbox(InMemoryInboxStorage::new()));
    let pending: Arc<dyn PendingDeliveryStorage> = Arc::new(poison::PoisonPending(
        InMemoryPendingDeliveryStorage::new(waddle_xmpp::pending_delivery::QuotaPolicy::Unlimited),
    ));
    let deps = Deps {
        mam_storage: Some(&mam),
        inbox_storage: Some(&inbox),
        pending_delivery_storage: Some(&pending),
        ..crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None)
    };
    let mut message = outgoing(jid("bob@example.com"));
    let mut preview = url::Url::parse(state.deps.auth_state.base_url.as_str()).expect("base URL");
    preview.set_path("/api/files/550e8400-e29b-41d4-a716-446655440000/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png");
    waddle_xmpp::xep::add_reference(
        &mut message,
        &waddle_xmpp::xep::Reference::data(preview.to_string()),
    );
    let database = state.deps.app_state.db_pool.global();
    super::sqlite_writes::install(database).await;
    let plan = plan_message_dispatch(&mut sender_machine(), message, &deps).await;
    assert_personal_writes(&plan.plan, &"alice@example.com".parse().expect("sender"));
    assert_personal_writes(&plan.plan, &"bob@example.com".parse().expect("recipient"));
    assert!(
        plan.plan.iter().any(|item| matches!(
            &item.effect,
            Effect::External(ExternalEffect::Delivery(
                ExternalDeliveryEffect::QueueOfflineDelivery {
                    prepared_notification: PreparedOfflineNotification::Prepared(_),
                    ..
                }
            ))
        )),
        "offline notification must be prepared to exercise concrete stores"
    );
    assert!(plan.plan.iter().any(|item| matches!(&item.effect,
        Effect::External(ExternalEffect::Direct(ExternalDirectEffect::LinkPreviewRefs { mutations })) if !mutations.is_empty())), "preview reference writes must be deferred from the database actor");
    super::sqlite_writes::assert_untouched(database).await;
}
