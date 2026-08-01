use super::*;

#[tokio::test]
async fn offline_recipient_pass_persists_archive_for_bare_jid_target() {
    // Sender pass already wrote alice's archive entry; the offline
    // recipient pass must additionally write bob's archive entry
    // because bob is local but has no available resources.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    // alice -> bob bare; no resources for bob registered.
    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "hello bob",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

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
        "headless recipient pass writes one archive entry under bob's bare"
    );
    assert_eq!(bob_archive.messages[0].body.as_deref(), Some("hello bob"));
}

#[tokio::test]
async fn offline_recipient_pass_persists_inbox_for_bare_jid_target() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "inbox row?",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let entries = inbox_concrete.list(&bob).await.expect("list");
    assert_eq!(
        entries.len(),
        1,
        "headless pass projects one inbox row keyed under bob"
    );
    assert_eq!(
        entries[0].partner, alice,
        "inbox row pairs (owner=bob, peer=alice)"
    );
}

#[tokio::test]
async fn route_to_connection_at_max_recursion_depth_drops_without_persistence() {
    // Direct unit test of the Codex-P1 recursion guard.
    // Calling `interpret_with_depth(...)` at
    // `MAX_RECIPIENT_PASS_DEPTH` simulates the inner-pass entry — a
    // `RouteToConnection` emitted from inside an in-flight headless
    // pass. The guard MUST short-circuit the entire arm (whether
    // the bare-JID has live targets or not), so no headless pass
    // runs and no recipient archive / inbox row is written.
    //
    // This pins the guard against regressions: removing or
    // weakening the depth check would let nested
    // `RouteToConnection` re-enter and cause duplicate persistence
    // in production. The test does not depend on which event the
    // transient SM's recipient pass actually emits.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "guard",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let outcome = interpret_with_depth(events, &deps, MAX_RECIPIENT_PASS_DEPTH).await;

    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert!(
        bob_archive.messages.is_empty(),
        "recursion guard at MAX_RECIPIENT_PASS_DEPTH prevents the headless \
         pass from running — bob's archive must remain empty"
    );
    let entries = inbox_concrete.list(&bob).await.expect("list");
    assert!(
        entries.is_empty(),
        "recursion guard prevents inbox projection at max depth"
    );
    assert!(
        outcome.frames.is_empty(),
        "recursion guard drops the route entirely — no frames produced"
    );
}

#[tokio::test]
async fn offline_recipient_pass_drops_send_stanza_no_wire() {
    // The transient SM emits `SendStanza` at the end of the
    // recipient pass (it's the wire-write effect for a live
    // connection). Without a live wire, those frames must not
    // bubble out into the *outer* `InterpretOutcome.frames`.
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
        jid("bob@example.com"),
        "drop wire",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let outcome = interpret(events, &deps).await;

    assert!(
        outcome.frames.is_empty(),
        "headless pass discards inner SendStanza frames; outer outcome stays empty"
    );
    assert!(
        outcome.feedback.is_empty(),
        "headless pass discards inner feedback events"
    );
    assert!(!outcome.close, "headless pass does not propagate close");
}

#[tokio::test]
async fn offline_recipient_pass_blocklist_loaded_from_storage_blocks_filtered_message() {
    // BlockingFilterHandler runs first in the recipient pass.
    // With alice on bob's blocklist, the message must be HALTed
    // before reaching ArchiveHandler — bob's archive stays empty.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking_concrete = Arc::new(InMemoryBlockingStorage::new());
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    blocking_concrete.set_blocklist(bob.clone(), vec![alice.clone()]);
    let blocking: Arc<dyn BlockingStorage> = blocking_concrete.clone();
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "blocked",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

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
        "BlockingFilterHandler halts the headless pass before ArchiveHandler — \
         no archive entry written for a blocked sender"
    );
}

#[tokio::test]
async fn offline_recipient_pass_blocklist_storage_error_skips_recipient_persistence() {
    // Fail-closed semantic (Copilot review on PR #275): when the
    // blocklist storage errors, the helper MUST skip the recipient
    // pass entirely — no archive, no inbox row — to preserve
    // XEP-0191 incoming-block enforcement. Mirrors PR13's bind-time
    // policy where a blocklist load error fails the bind.
    // Degrading to `Blocklist::empty()` would silently allow blocked
    // senders into the recipient's MAM / inbox.
    use async_trait::async_trait;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
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
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(FailingBlocking);
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "fail-closed",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert!(
        bob_archive.messages.is_empty(),
        "blocklist load error fails closed — recipient archive NOT written"
    );
    let entries = inbox_concrete.list(&bob).await.expect("list");
    assert!(
        entries.is_empty(),
        "blocklist load error fails closed — recipient inbox NOT written"
    );
}

#[tokio::test]
async fn xep_0359_offline_recipient_pass_emits_recipient_archive_with_recipient_stanza_id() {
    // L4 wire-trace integration: drive alice's *live* sender pass
    // through the dispatcher chain, then take alice's
    // RouteToConnection event and feed it into the interpreter.
    // The headless offline-recipient pass should write bob's
    // archive entry stamped `<stanza-id by='bob@example.com'>`
    // and project bob's inbox keyed (bob, alice). No frames are
    // produced for bob (no wire).
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::protocol::handlers::register_default_message_handlers;
    use waddle_xmpp::protocol::InboundEvent;
    use waddle_xmpp::protocol::InboundFrame;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    // ---- alice/web: live SM driving the sender pass ----
    let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let alice_bare: jid::BareJid = "alice@example.com".parse().expect("bare");

    let mut sender_dispatch = StanzaDispatcher::new();
    register_default_message_handlers(&mut sender_dispatch);
    let mut alice_sm = XmppStateMachine::new("example.com", sender_dispatch);
    alice_sm.transition_to_ready(alice_web.clone(), false);

    let mut wire_msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(bob.clone())));
    wire_msg.from = Some(jid::Jid::from(alice_web.clone()));
    wire_msg.type_ = xmpp_parsers::message::MessageType::Chat;
    wire_msg.id = Some(xmpp_parsers::message::Id("wire-id".to_string()));
    wire_msg.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "wire-trace body".to_string(),
    );

    let alice_events = alice_sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(
        Box::new(Stanza::Message(wire_msg)),
    )));

    // ---- shared storage + dispatcher for the headless pass ----
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    // The headless pass constructs a transient `XmppStateMachine`
    // for bob, cloning this dispatcher so the recipient handler
    // chain runs against bob's bare JID. XEP-0359 stanza-id
    // determinism is owned by the per-machine `IdGenerator` (see
    // `XmppStateMachine::with_id_gen`), not by the dispatcher
    // itself — this fixture relies on uniqueness rather than
    // deterministic ids.
    let mut headless_dispatch = StanzaDispatcher::new();
    register_default_message_handlers(&mut headless_dispatch);
    let dispatcher = Arc::new(headless_dispatch);
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    // Run the interpreter on alice's full event batch. The
    // ArchiveDirect for alice's bare lands in alice's archive,
    // ProjectInbox for (alice, bob) lands in alice's inbox, and
    // the bare-JID RouteToConnection for bob with no live
    // resources triggers the headless pass.
    let outcome = interpret(alice_events, &deps).await;

    // alice's MAM has 1 entry; <stanza-id by='alice@example.com'>
    // present.
    let alice_archive = mam
        .query_messages(
            &alice_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query alice");
    assert_eq!(
        alice_archive.messages.len(),
        1,
        "alice archive has one entry"
    );
    assert!(
        alice_archive.messages[0]
            .stanza_xml
            .as_deref()
            .map(|xml| xml.contains("by='alice@example.com'"))
            .unwrap_or(false),
        "alice archive entry carries XEP-0359 <stanza-id by='alice@example.com'/>: \
         {:?}",
        alice_archive.messages[0].stanza_xml
    );

    // bob's MAM has 1 entry; <stanza-id by='bob@example.com'>
    // present (recipient-side stamp by the headless pass).
    let bob_archive = mam
        .query_messages(
            &bob,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "headless pass writes one archive entry for bob"
    );
    assert!(
        bob_archive.messages[0]
            .stanza_xml
            .as_deref()
            .map(|xml| xml.contains("by='bob@example.com'"))
            .unwrap_or(false),
        "bob archive entry carries XEP-0359 <stanza-id by='bob@example.com'/>: \
         {:?}",
        bob_archive.messages[0].stanza_xml
    );

    // bob's inbox has 1 row at (bob, alice).
    let bob_inbox = inbox_concrete.list(&bob).await.expect("inbox bob");
    assert_eq!(
        bob_inbox.len(),
        1,
        "headless pass projects exactly one inbox row for bob"
    );
    assert_eq!(bob_inbox[0].partner, alice_bare);

    // No frames for bob — the headless pass discards any inner
    // SendStanza. The outer outcome may still carry alice's own
    // sender-side frames (none in this fixture because there's no
    // alice connection registered), so this asserts only the
    // negative: no frame addressed 'to=bob' leaks out.
    for frame in &outcome.frames {
        assert!(
            !frame.contains("to='bob@example.com'"),
            "headless pass must not produce wire frames for offline bob; got: {frame}"
        );
    }
}

#[tokio::test]
async fn offline_recipient_pass_skipped_for_remote_domain() {
    // bob@other.example with `local_domain="example.com"` -> drop,
    // no recipient pass run, no archive, no inbox.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher);

    let msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@other.example.com"),
        "remote",
    );
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@other.example.com"
            .parse::<jid::Jid>()
            .expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
        call_setup: None,
    }];
    let _ = interpret(events, &deps).await;

    let bob_remote: jid::BareJid = "bob@other.example.com".parse().expect("bare");
    let bob_archive = mam
        .query_messages(
            &bob_remote,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert!(
        bob_archive.messages.is_empty(),
        "cross-domain bare JID drops without running the headless pass"
    );
    let entries = inbox_concrete.list(&bob_remote).await.expect("list");
    assert!(
        entries.is_empty(),
        "cross-domain bare JID drops without inbox projection"
    );
}
