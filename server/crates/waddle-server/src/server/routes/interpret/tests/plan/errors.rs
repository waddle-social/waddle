use super::*;

#[tokio::test]
async fn recipient_block_error_is_retained_as_the_plans_error_reply() {
    let registry = test_registry();
    let mam: Arc<dyn MamStorage> = Arc::new(poison::PoisonMam(InMemoryMamStorage::new()));
    let inbox: Arc<dyn InboxStorage> = Arc::new(poison::PoisonInbox(InMemoryInboxStorage::new()));
    let blocked = InMemoryBlockingStorage::new();
    blocked.set_blocklist(
        "bob@example.com".parse().expect("recipient"),
        vec!["alice@example.com".parse().expect("sender")],
    );
    let blocking: Arc<dyn BlockingStorage> = Arc::new(blocked);
    let dispatcher = pipelined_dispatcher();
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let sender = "alice@example.com/web".parse().expect("sender");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &sender, tx).await;
    let deps = Deps {
        user_registry: Some(&user_registry),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };
    let plan = plan_message_dispatch(
        &mut sender_machine(),
        outgoing(jid("bob@example.com")),
        &deps,
    )
    .await;
    assert!(
        matches!(plan.error_reply, Some(Stanza::Message(message)) if message.type_ == XmppMessageType::Error && message.to == Some(jid("alice@example.com/web")))
    );
}
