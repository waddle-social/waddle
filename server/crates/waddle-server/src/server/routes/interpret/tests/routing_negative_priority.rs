use super::*;

// ---------------------------------------------------------------------
// #1266 item 4 — RFC 6121 §8.5.2.1.1: bare-JID delivery MUST NOT reach
// resources that advertised a negative presence priority.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_to_connection_bare_jid_skips_negative_priority_resources() {
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_phone, phone_tx).await;
    // desk explicitly opts out of bare-JID delivery (priority -1);
    // phone is connected but has not sent presence (tier-2 fallback
    // territory).
    registry.update_presence(&bob_desk, true, -1);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "hi bare",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "RFC 6121 §8.5.2.1.1: negative-priority resource must not receive \
         bare-JID delivery"
    );
    assert_eq!(
        drain_inbound(&mut phone_rx).len(),
        1,
        "presence-deferred sibling still receives via the tier-2 fallback"
    );
}

#[tokio::test]
async fn route_to_connection_bare_jid_all_negative_priority_goes_offline() {
    // A user whose only resources advertise negative priority is
    // treated as offline for bare-JID delivery (§8.5.2.1.1 →
    // "SHOULD store offline"): the headless pass persists instead of
    // delivering.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, -1);

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    );

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "store me",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "negative-priority resource must not receive the message"
    );
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "message stored offline instead of delivered to the negative resource"
    );
}
