use super::*;

// ---------------------------------------------------------------------
// #1244 — RFC 6121 §8.5.3.2.1: full-JID DM with no matching resource
// falls back to bare-JID delivery semantics instead of dropping.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_full_jid_dm_offline_resource_falls_back_to_other_live_resource() {
    // Alice keeps replying to bob@x/old-resource after Bob reconnected
    // under /desk. RFC 6121 §8.5.3.2.1: with no resource matching the
    // full JID, treat the stanza as addressed to the bare JID — /desk
    // must receive it (previously: silent drop).
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 0);

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
        jid("bob@example.com/gone"),
        "hi bob",
    );
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/gone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    assert!(
        outcome.frames.is_empty(),
        "fallback delivery must not synthesize an error to the sender"
    );

    let delivered = drain_inbound(&mut desk_rx);
    assert_eq!(
        delivered.len(),
        1,
        "RFC 6121 §8.5.3.2.1: bare-JID fallback delivers to bob's live resource"
    );
    assert_eq!(
        delivered[0].kind,
        DeliveryKind::DirectFrame,
        "fallback goes through the shared recipient pass (processed copy)"
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
        "recipient pass ran exactly once for the fallback delivery"
    );
}

#[tokio::test]
async fn route_full_jid_dm_no_resources_stores_offline() {
    // Full-JID DM, recipient has no resources at all: §8.5.3.2.1 →
    // §8.5.2 → offline handling (headless recipient pass persists
    // archive + inbox). Previously the message vanished.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com/gone"),
        "offline?",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/gone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

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
        "full-JID DM to a fully-offline user must be stored, not dropped"
    );
}

#[tokio::test]
async fn route_full_jid_dm_to_detached_resource_runs_recipient_pipeline() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-phone-stream", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com/phone"),
        "resume me",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/phone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    // XEP-0313 §6.1: the recipient archive captured the message.
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
        "detached full-JID DM must land in the recipient's archive"
    );

    // XEP-0359 §5: the queued replay copy is the PROCESSED stanza and
    // carries the recipient <stanza-id by='bob@example.com'/>.
    let session = sm
        .peek_session("bob-phone-stream")
        .await
        .expect("peek ok")
        .expect("session present");
    assert_eq!(
        session.unacked_stanzas.len(),
        1,
        "processed DM queued for XEP-0198 replay"
    );
    let queued_element: Element = session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("queued stanza XML parses");
    let queued =
        xmpp_parsers::message::Message::try_from(queued_element).expect("queued message parses");
    let by: jid::Jid = "bob@example.com".parse().expect("jid");
    let recipient_stanza_id = waddle_xmpp_core::xep0359::extract_stanza_id_by(&queued, &by);
    assert!(
        recipient_stanza_id.is_some(),
        "replay copy must carry the recipient-side stanza-id (XEP-0359 §3); \
         payloads: {:?}",
        queued.payloads
    );
    assert_eq!(
        recipient_stanza_id.as_deref(),
        Some(bob_archive.messages[0].id.as_str()),
        "wire stanza-id and archive row id must agree"
    );
}
