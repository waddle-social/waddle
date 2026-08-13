use super::*;

// -----------------------------------------------------------------
// #1106 — shared fan-out recipient pass: blocklist-load failure
// -----------------------------------------------------------------

/// BlockingStorage stub whose reads always fail, simulating a
/// transient storage outage during the shared fan-out pass.
struct FailingBlockingStorage;

#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for FailingBlockingStorage {
    async fn list_blocked_jids(
        &self,
        _user: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Err(waddle_xmpp::xep::xep0191::BlockingStorageError::new(
            std::io::Error::other("storage down"),
        ))
    }

    async fn list_blocked_jid_entries(
        &self,
        _user: &jid::BareJid,
    ) -> Result<Vec<jid::Jid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        Err(waddle_xmpp::xep::xep0191::BlockingStorageError::new(
            std::io::Error::other("storage down"),
        ))
    }
}

#[tokio::test]
async fn fanout_pass_blocklist_failure_falls_back_to_legacy_per_resource_delivery() {
    // A transient blocklist-storage error must not drop a DM to LIVE
    // recipients: the legacy per-resource PeerStanza path still runs
    // each recipient connection's own state machine, whose bind-time
    // blocklist snapshot keeps XEP-0191 enforcement intact.
    use waddle_xmpp::registry::DeliveryKind;

    let registry = ConnectionRegistry::new();
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    // ADR-0017 Slice 1: bare-JID selection reads the actor tree, so register
    // bob into both tiers. bob sends no presence, so tier-2 `GetResources`
    // (the bound-without-presence fallback) resolves him as the live target.
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let blocking: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        Arc::new(FailingBlockingStorage);
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        connection_registry: &registry,
        user_registry: Some(&user_registry),
        sm_session_registry: None,
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: None,
        web_socket_state: None,
        authenticated_principal: None,
        local_domain: "example.com",
        blocking_storage: Some(&blocking),
        message_dispatcher: Some(&dispatcher),
        pending_delivery_storage: None,
        ordered_relay_origin: None,
        sfu: None,
        ingress_effect_capture: None,
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "must arrive",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), bob_rx.recv())
        .await
        .expect("delivery must not time out")
        .expect("channel open");
    assert_eq!(
        delivered.kind,
        DeliveryKind::PeerStanza,
        "fallback delivers via the legacy per-resource recipient pass"
    );
    let Stanza::Message(delivered_msg) = delivered.stanza else {
        panic!("expected message stanza");
    };
    assert_eq!(
        delivered_msg.bodies.values().next().map(|b| b.as_str()),
        Some("must arrive")
    );
}

#[tokio::test]
async fn fanout_pass_applies_archive_id_rewrite_to_the_delivered_stanza() {
    // XEP-0359 live/MAM id parity under origin-id retry: when the
    // recipient archive dedupes the store to an EXISTING row (same
    // origin-id already archived), the resulting ArchiveIdRewrite must
    // reach the wire copy the shared fan-out pass delivers — otherwise
    // live resources see a recipient <stanza-id/> that no archive row
    // carries, breaking client-side live/MAM dedupe.
    use waddle_xmpp::registry::DeliveryKind;
    use waddle_xmpp_core::xep0359::{build_origin_id_element, extract_stanza_id_by};

    let registry = ConnectionRegistry::new();
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    // ADR-0017 Slice 1: bare-JID selection reads the actor tree; bob is live
    // (bound without presence), resolved via tier-2 `GetResources`.
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let mam: Arc<dyn MamStorage> =
        Arc::new(waddle_xmpp::mam::storage::InMemoryMamStorage::default());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let blocking: Arc<dyn waddle_xmpp::xep::xep0191::BlockingStorage> =
        Arc::new(waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps_with_user_registry(
        &registry,
        &user_registry,
        &mam,
        &inbox,
        &blocking,
        &dispatcher,
    );

    let dm = || {
        let mut m = chat_msg(
            jid("alice@example.com/web"),
            jid("bob@example.com"),
            "retry me",
        );
        m.payloads.push(build_origin_id_element("origin-retry-1"));
        m
    };

    // First delivery: archives a row under bob's recipient stamp.
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(dm())),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    // Drain the first delivery.
    while bob_rx.try_recv().is_ok() {}

    // Retry with the same origin-id: the archive store dedupes to the
    // existing row and reports its id via ArchiveIdRewrite.
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(dm())),
            call_setup: None,
        }],
        &deps,
    )
    .await;
    let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), bob_rx.recv())
        .await
        .expect("second delivery must not time out")
        .expect("channel open");
    assert_eq!(
        delivered.kind,
        DeliveryKind::DirectFrame,
        "shared fan-out pass delivers the processed stanza directly"
    );
    let Stanza::Message(delivered_msg) = delivered.stanza else {
        panic!("expected message stanza");
    };

    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob archive");
    assert_eq!(
        archive.messages.len(),
        1,
        "origin-id retry dedupes to one row"
    );
    let archived_id = archive.messages[0].id.clone();

    let delivered_stanza_id = extract_stanza_id_by(&delivered_msg, &jid::Jid::from(bob_bare));
    assert_eq!(
        delivered_stanza_id.as_deref(),
        Some(archived_id.as_str()),
        "the delivered recipient <stanza-id/> must match the deduped archive row"
    );
}
