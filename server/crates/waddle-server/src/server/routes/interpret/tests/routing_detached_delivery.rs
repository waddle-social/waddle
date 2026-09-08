use super::*;
use crate::ingress::IngressEffectCapture;

// ---------------------------------------------------------------------
// XEP-0191 fail-closed: a blocklist load failure must never let the
// raw (unfiltered) stanza into a detached XEP-0198 replay buffer —
// replay writes stored XML verbatim with no recipient pass.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_full_jid_dm_to_detached_drops_when_blocklist_load_fails() {
    use async_trait::async_trait;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::{BlockingStorage, BlockingStorageError};

    #[derive(Debug, thiserror::Error)]
    #[error("simulated blocking storage failure")]
    struct SimulatedFailure;

    struct FailingBlocking;
    #[async_trait]
    impl BlockingStorage for FailingBlocking {
        async fn list_blocked_jids(
            &self,
            _: &jid::BareJid,
        ) -> Result<Vec<jid::BareJid>, BlockingStorageError> {
            Err(BlockingStorageError::new(SimulatedFailure))
        }
    }

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-blocked-stream", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com/phone"),
        "maybe blocked",
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

    let session = sm
        .peek_session("bob-blocked-stream")
        .await
        .expect("peek ok")
        .expect("session present");
    assert!(
        session.unacked_stanzas.is_empty(),
        "blocklist load failure must fail closed: no raw stanza may be \
         queued for XEP-0198 replay"
    );
}

#[tokio::test]
async fn route_bare_jid_dm_to_detached_only_recipient_runs_recipient_pipeline() {
    // Qodo review on PR #1272: a bare-JID DM whose recipient has ONLY
    // detached XEP-0198 resources must run the shared recipient pass
    // (recipient MAM row + stamped replay copy), not queue the raw
    // pre-pass stanza.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-bare-detached", &bob_phone))
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
        jid("bob@example.com"),
        "bare detached",
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
        "detached-only bare-JID DM must land in the recipient's archive"
    );

    let session = sm
        .peek_session("bob-bare-detached")
        .await
        .expect("peek ok")
        .expect("session present");
    assert_eq!(session.unacked_stanzas.len(), 1);
    let queued_element: Element = session.unacked_stanzas[0]
        .stanza_xml
        .parse()
        .expect("queued stanza XML parses");
    let queued =
        xmpp_parsers::message::Message::try_from(queued_element).expect("queued message parses");
    let by: jid::Jid = "bob@example.com".parse().expect("jid");
    assert!(
        waddle_xmpp_core::xep0359::extract_stanza_id_by(&queued, &by).is_some(),
        "detached-only replay copy must be the PROCESSED (stamped) stanza"
    );
}

#[tokio::test]
async fn detached_dm_append_records_the_actual_sm_stream() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("captured-dm-stream", &bob_phone))
        .await
        .expect("store detached session");
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let capture = IngressEffectCapture::new();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ingress_effect_capture: Some(capture.clone()),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob_phone),
            stanza: Box::new(Stanza::Message(chat_msg(
                jid("alice@example.com/web"),
                jid("bob@example.com/phone"),
                "detached",
            ))),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert!(capture.snapshot().intents.iter().any(|intent| {
        matches!(
            intent,
            IngressEffectIntent::RecipientSmAppend { stream, .. }
                if stream.as_str() == "captured-dm-stream"
        )
    }));
    assert!(
        capture.snapshot().intents.iter().any(|intent| matches!(intent, IngressEffectIntent::RouteDirect { recipient, fanout, .. } if *recipient == "bob@example.com".parse::<jid::BareJid>().expect("bare") && *fanout == vec!["bob@example.com/phone".parse::<jid::FullJid>().expect("full")])),
        "detached full-JID DM must also record the accepted direct-route target"
    );
}

#[tokio::test]
async fn detached_non_dm_append_records_the_actual_sm_stream() {
    use waddle_xmpp::stream_management::SmSessionRegistry;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("captured-presence-stream", &bob_phone))
        .await
        .expect("store detached session");
    let capture = IngressEffectCapture::new();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ingress_effect_capture: Some(capture.clone()),
        ..Deps::registry_only(&registry)
    };
    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid("alice@example.com/web"));
    presence.to = Some(jid::Jid::from(bob_phone));

    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/phone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Presence(presence)),
            call_setup: None,
        }],
        &deps,
    )
    .await;

    assert!(capture.snapshot().intents.iter().any(|intent| matches!(
        intent,
        IngressEffectIntent::RecipientSmAppend { stream, .. }
            if stream.as_str() == "captured-presence-stream"
    )));
}

#[tokio::test]
async fn route_bare_jid_dm_from_blocked_sender_to_detached_only_recipient_is_filtered() {
    // The recipient (only detached) has blocked the sender: the shared
    // pass must halt the message (nothing queued for replay) and bounce
    // <service-unavailable/> to the sender per XEP-0191.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::stream_management::SmSessionRegistry;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    sm.store_session(detached_dm_session("bob-blocked-bare", &bob_phone))
        .await
        .expect("store detached session");

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking_concrete = Arc::new(InMemoryBlockingStorage::new());
    blocking_concrete.set_blocklist(
        "bob@example.com".parse::<jid::BareJid>().expect("bare"),
        vec!["alice@example.com".parse::<jid::BareJid>().expect("bare")],
    );
    let blocking: Arc<dyn BlockingStorage> = blocking_concrete;
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        sm_session_registry: Some(&sm),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "should not pass",
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

    let session = sm
        .peek_session("bob-blocked-bare")
        .await
        .expect("peek ok")
        .expect("session present");
    assert!(
        session.unacked_stanzas.is_empty(),
        "blocked sender's message must not reach the detached replay buffer"
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
    assert!(
        bob_archive.messages.is_empty(),
        "blocked sender's message must not be archived for the recipient"
    );
}
