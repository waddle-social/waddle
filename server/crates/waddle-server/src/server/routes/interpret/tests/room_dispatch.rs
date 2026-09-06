#[cfg(feature = "clustering")]
use super::super::room_dispatch::capture_delivered_remote_room_route;
use super::super::room_dispatch::push_sender_error_reply;
use super::*;
use crate::ingress_shadow::IngressEffectCapture;

// XEP-0372 — RequestEnrichment callback round-trip
// -----------------------------------------------------------------

#[test]
fn extension_waddle_scope_matches_managed_room_context() {
    let managed_room: BareJid = "general@muc.example.com".parse().expect("room jid");
    assert_eq!(waddle_id_for_room_jid(&managed_room).as_str(), "space");

    let unmanaged_room: BareJid = "conference.example.com".parse().expect("room jid");
    assert_eq!(waddle_id_for_room_jid(&unmanaged_room).as_str(), "default");
}

#[cfg(feature = "clustering")]
#[test]
fn delivered_remote_room_dispatch_captures_frozen_route_intent() {
    let capture = IngressEffectCapture::new(None);
    let room: BareJid = "remote@muc.example.com".parse().expect("room");
    let relay_target = waddle_xmpp::ingress::RelayTargetIdentity::owner_node("node-b", "epoch-b");

    capture_delivered_remote_room_route(&capture, &room, relay_target.clone());

    assert!(capture
        .snapshot()
        .intents
        .contains(&IngressEffectIntent::DispatchToRoomRemote { room, relay_target }));
}

// #229 PR18 — DispatchToRoom interpreter arm runs the room handler
// chain (Q7 option C). The end-to-end semantics (managed-room owner
// check, rich-target validation, MAM archive, retraction
// tombstones, durable-recipient inbox projection, occupant fan-out) are
// exercised by the integration tests in
// `crates/waddle-server/tests/*_ws.rs`; the L1 unit test below pins
// the chain wiring against the lightweight in-process `Deps` shape.
// -----------------------------------------------------------------

/// Without `web_socket_state` the arm logs a warn and drops the
/// event without panicking — production must wire `web_socket_state`
/// via [`super::super::websocket::build_interpret_deps`].
#[tokio::test]
async fn dispatch_to_room_drops_when_no_web_socket_state_in_deps() {
    let registry = ConnectionRegistry::new();
    let room_jid: jid::BareJid = "testroom@muc.example.com".parse().expect("parse room jid");
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    message.from = Some(
        "alice@example.com/web"
            .parse::<jid::FullJid>()
            .map(jid::Jid::from)
            .expect("from"),
    );

    let events = vec![OutboundEvent::DispatchToRoom {
        room: room_jid,
        message: Box::new(message),
    }];
    let outcome = interpret(events, &Deps::registry_only(&registry)).await;
    assert!(outcome.frames.is_empty());
    assert!(!outcome.close);
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_to_room_fanout_span_and_latency_cover_recipient_enqueues() {
    use waddle_xmpp::muc::room_actor::Join;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::{Affiliation, Role};

    let metric_guard = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let room_jid: jid::BareJid = "trace@muc.example.com".parse().expect("room JID");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice JID");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob JID");
    let room = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-trace".to_string(),
            channel_id: "c-trace".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");
    for (nick, real_jid) in [("alice", alice.clone()), ("bob", bob.clone())] {
        room.ask(Join {
            nick: nick.to_string(),
            real_jid,
            role: Role::Participant,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join room");
    }

    let (alice_tx, mut alice_rx) = tokio::sync::mpsc::channel(8);
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(
        &state.deps.protocol.connection_registry,
        &state.deps.protocol.user_registry,
        &alice,
        alice_tx,
    )
    .await;
    register_into_both_tiers(
        &state.deps.protocol.connection_registry,
        &state.deps.protocol.user_registry,
        &bob,
        bob_tx,
    )
    .await;

    let capture = IngressEffectCapture::new(None);
    let deps = Deps {
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
        connection_registry: &state.deps.protocol.connection_registry,
        user_registry: Some(&state.deps.protocol.user_registry),
        sm_session_registry: Some(&state.deps.protocol.sm_session_registry),
        mam_storage: Some(&state.deps.protocol.mam_storage),
        inbox_storage: Some(&state.deps.protocol.inbox_storage),
        extension_manager: Some(&state.deps.protocol.extension_manager),
        room_registry: Some(&state.deps.protocol.room_registry),
        web_socket_state: Some(state.as_ref()),
        authenticated_principal: None,
        local_domain: state.deps.auth_state.xmpp_domain.as_str(),
        blocking_storage: Some(&state.deps.protocol.blocking_storage),
        message_dispatcher: Some(&state.deps.protocol.dispatcher),
        pending_delivery_storage: Some(&state.deps.protocol.pending_delivery_storage),
        ordered_relay_origin: None,
        sfu: None,
        ingress_effect_capture: Some(capture.clone()),
    };
    let mut message = Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(jid::Jid::from(alice));
    message.type_ = XmppMessageType::Groupchat;
    message.id = Some(xmpp_parsers::message::Id("fanout-production-1".into()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "sensitive production fanout body".to_string(),
    );

    let bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(CaptureWriter(Arc::clone(&bytes)))
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let outcome = interpret(
        vec![OutboundEvent::DispatchToRoom {
            room: room_jid,
            message: Box::new(message),
        }],
        &deps,
    )
    .await;

    assert!(outcome.frames.is_empty());
    assert!(
        alice_rx.try_recv().is_ok(),
        "sender reflection was enqueued"
    );
    assert!(
        bob_rx.try_recv().is_ok(),
        "recipient reflection was enqueued"
    );
    let output = String::from_utf8(bytes.lock().expect("capture lock").clone())
        .expect("captured tracing is UTF-8");
    let fanout_span = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|event| {
            event
                .get("span")
                .filter(|span| {
                    span.get("name").and_then(serde_json::Value::as_str) == Some("xmpp.muc.fanout")
                        && span.get("recipients").is_some()
                })
                .cloned()
                .or_else(|| {
                    event
                        .get("spans")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|spans| {
                            spans
                                .iter()
                                .find(|span| {
                                    span.get("name").and_then(serde_json::Value::as_str)
                                        == Some("xmpp.muc.fanout")
                                        && span.get("recipients").is_some()
                                })
                                .cloned()
                        })
                })
        })
        .expect("captured event belongs to xmpp.muc.fanout span");
    assert_eq!(
        fanout_span
            .get("message_id")
            .and_then(serde_json::Value::as_str),
        Some("fanout-production-1")
    );
    assert_eq!(
        fanout_span.get("room").and_then(serde_json::Value::as_str),
        Some("trace@muc.example.com")
    );
    assert_eq!(
        fanout_span
            .get("recipients")
            .and_then(serde_json::Value::as_u64),
        Some(2)
    );
    assert!(
        !output.contains("sensitive production fanout body"),
        "message bodies must never be tracing fields: {output}"
    );
    assert_eq!(
        metric_guard.histogram_count("xmpp.muc.fanout.latency", &[]),
        Some(1),
        "one sample is recorded after both enqueue attempts complete"
    );
    assert_eq!(
        metric_guard
            .metric_unit("xmpp.muc.fanout.latency")
            .as_deref(),
        Some("ms")
    );
    assert!(capture.snapshot().intents.iter().any(|intent| {
        matches!(
            intent,
            IngressEffectIntent::RouteMucGroupchat { room, occupants, .. }
                if room.to_string() == "trace@muc.example.com" && occupants.len() == 2
        )
    }));
}

#[test]
fn successful_room_error_reply_records_error_intent() {
    let registry = ConnectionRegistry::new();
    let capture = IngressEffectCapture::new(None);
    let deps = Deps {
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
        connection_registry: &registry,
        user_registry: None,
        sm_session_registry: None,
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: None,
        web_socket_state: None,
        authenticated_principal: None,
        local_domain: "example.com",
        blocking_storage: None,
        message_dispatcher: None,
        pending_delivery_storage: None,
        ordered_relay_origin: None,
        sfu: None,
        ingress_effect_capture: Some(capture.clone()),
    };
    let room: jid::BareJid = "room@muc.example.com".parse().expect("room");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender");
    let incoming = chat_msg(
        jid::Jid::from(sender.clone()),
        jid::Jid::from(room.clone()),
        "bad",
    );
    let mut outcome = InterpretOutcome::default();
    let expected_error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Cancel,
        xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound,
        "en",
        "missing",
    );
    push_sender_error_reply(
        &deps,
        &mut outcome,
        &incoming,
        &room,
        &sender,
        expected_error.clone(),
    );
    assert_eq!(outcome.frames.len(), 1);
    assert!(capture.snapshot().intents.iter().any(|intent| {
        matches!(
            intent,
            IngressEffectIntent::ErrorReply { recipient, error }
                if *recipient == sender
                    && *error == waddle_xmpp::ingress::FrozenStanzaError::from_xmpp(&expected_error)
                        .expect("server-built stanza error should freeze")
        )
    }));
}

#[tokio::test]
async fn extension_room_message_dispatches_threaded_muc_message() {
    use waddle_extensions::{
        DisplayText, FullJidValue, MessageMarkupKind, MessageMarkupSpan, ReplyTarget, RoomJid,
        StanzaId, ThreadId,
    };

    let registry = ConnectionRegistry::new();
    // ADR-0017 Slice 3: groupchat reflection delivers through the authoritative
    // actor (`deliver_peer_to_full`), so occupants must be in both tiers.
    let user_registry = waddle_xmpp::registry::UserRegistryActor::spawn(
        waddle_xmpp::registry::UserRegistryActor::new(),
    );
    let room_jid: jid::BareJid = "chat@muc.example.com".parse().expect("room jid");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
    let bot: jid::FullJid = "chat@example.com/bot".parse().expect("bot jid");
    let (alice_tx, mut alice_rx) = tokio::sync::mpsc::channel(8);
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &alice, alice_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let occupants = vec![
        OccupantSnapshot {
            full_jid: alice.clone(),
            nick: "alice".to_string(),
            affiliation: waddle_xmpp::Affiliation::Member,
            role: waddle_xmpp::Role::Participant,
        },
        OccupantSnapshot {
            full_jid: bob.clone(),
            nick: "bob".to_string(),
            affiliation: waddle_xmpp::Affiliation::Member,
            role: waddle_xmpp::Role::Participant,
        },
        OccupantSnapshot {
            full_jid: bot.clone(),
            nick: "waddle".to_string(),
            affiliation: waddle_xmpp::Affiliation::Member,
            role: waddle_xmpp::Role::Participant,
        },
    ];
    let response = ExtensionRoomMessage {
        body: DisplayText::new("bot answer").expect("body"),
        room: RoomJid::new(room_jid.to_string()).expect("room"),
        preferred_nick: None,
        bot_hat_label: None,
        stanza_id: None,
        thread_id: Some(ThreadId::new("root-msg").expect("thread")),
        reply_to: Some(ReplyTarget {
            id: StanzaId::new("root-msg").expect("reply id"),
            to: Some(FullJidValue::new(alice.to_string()).expect("reply to")),
        }),
        markup: vec![MessageMarkupSpan {
            kind: MessageMarkupKind::Blockquote,
            start: 0,
            end: 10,
        }],
        extensions: None,
    };

    let test_secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
        b"test-occupant-id-secret-32-bytes-long".to_vec(),
    )
    .expect("test secret meets length floor");
    let outcome = dispatch_bot_groupchat_response(
        &Deps::registry_with_user_registry(&registry, &user_registry),
        BotGroupchatDispatch {
            room_jid: &room_jid,
            occupants: &occupants,
            durable_recipient_bare_jids: &[],
            sender_full: &bot,
            room_actor: None,
            room_moderated: false,
            room_occupants_may_change_subject: false,
            room_members_only: false,
            pin_permission: waddle_xmpp::muc::PinPermission::default(),
            dispatch_timestamp: 1777629203,
            recursion_depth: 0,
            occupant_id_secret: &test_secret,
        },
        response,
    )
    .await;
    let outcome = outcome.expect("bot dispatch should succeed").outcome;

    assert!(outcome.frames.is_empty());
    assert!(!outcome.close);

    let alice_delivered = drain_inbound(&mut alice_rx);
    let bob_delivered = drain_inbound(&mut bob_rx);
    assert_eq!(alice_delivered.len(), 1);
    assert_eq!(bob_delivered.len(), 1);

    let Stanza::Message(message) = &alice_delivered[0].stanza else {
        panic!("expected bot groupchat message");
    };
    assert_eq!(message.type_, xmpp_parsers::message::MessageType::Groupchat);
    assert_eq!(
        message.from.as_ref().map(ToString::to_string),
        Some(format!("{room_jid}/waddle"))
    );
    assert_eq!(
        message.thread.as_ref().map(|thread| thread.id.as_str()),
        Some("root-msg")
    );
    assert_eq!(
        message.bodies.get("").map(|body| body.as_str()),
        Some("bot answer")
    );
    let markup = message
        .payloads
        .iter()
        .find(|payload| payload.is("markup", waddle_xmpp::xep::NS_MESSAGE_MARKUP))
        .expect("markup payload");
    let quote = markup
        .get_child("bquote", waddle_xmpp::xep::NS_MESSAGE_MARKUP)
        .expect("blockquote markup");
    assert_eq!(quote.attr("start"), Some("0"));
    assert_eq!(quote.attr("end"), Some("10"));
    let reply = parse_reply_from_message(message).expect("reply payload");
    assert_eq!(reply.id, "root-msg");
    assert_eq!(reply.to, None);
    assert!(
        !message
            .payloads
            .iter()
            .any(|payload| payload.ns() == "urn:waddle:forums:0"),
        "plain MUC bot responses must not reuse forum metadata"
    );
}

#[test]
fn groupchat_reply_to_attr_only_preserves_room_occupant_jids() {
    let room: BareJid = "chat@muc.example.com".parse().expect("room");

    assert_eq!(
        room_scoped_reply_to_attr("chat@muc.example.com/alice", &room),
        Some(
            "chat@muc.example.com/alice"
                .parse::<Jid>()
                .expect("occupant jid")
        )
    );
    assert_eq!(
        room_scoped_reply_to_attr("alice@example.com/web", &room),
        None
    );
    assert_eq!(room_scoped_reply_to_attr("not a jid", &room), None);
}

#[test]
fn message_thread_id_reads_existing_forum_reply_without_rfc_thread() {
    let xml = r#"<message xmlns='jabber:client' id='child'>
        <thread-reply xmlns='urn:waddle:forums:0' thread-id='root-msg'/>
    </message>"#;
    let element: Element = xml.parse().expect("element");
    let message = Message::try_from(element).expect("message");
    assert_eq!(message_thread_id(&message).as_deref(), Some("root-msg"));
}

#[test]
fn thread_create_source_is_normalized_for_inbox_projection() {
    let mut message = Message::new(Some(Jid::from(
        "chat@muc.example.com"
            .parse::<jid::BareJid>()
            .expect("room jid"),
    )));
    message.id = Some(xmpp_parsers::message::Id("live-forum-root".to_string()));
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    set_thread_create(&mut message, &ThreadCreate::new("Live forum root"));

    let thread_id = normalize_thread_create_source(&mut message);

    assert_eq!(thread_id.as_deref(), Some("live-forum-root"));
    assert_eq!(
        message.thread.as_ref().map(|thread| thread.id.as_str()),
        Some("live-forum-root")
    );
    assert!(matches!(
        extract_forum_action(&message),
        Some(ForumAction::CreateThread(_))
    ));
}

#[test]
fn bot_nick_avoids_existing_occupant_collision() {
    let occupants = vec![
        OccupantSnapshot {
            full_jid: "alice@example.com/web".parse().expect("alice jid"),
            nick: "waddle".to_string(),
            affiliation: waddle_xmpp::Affiliation::Member,
            role: waddle_xmpp::Role::Participant,
        },
        OccupantSnapshot {
            full_jid: "bob@example.com/web".parse().expect("bob jid"),
            nick: "waddle-2".to_string(),
            affiliation: waddle_xmpp::Affiliation::Member,
            role: waddle_xmpp::Role::Participant,
        },
    ];

    assert_eq!(available_bot_nick(&occupants), "waddle-3");
}

#[test]
fn bot_nick_uses_extension_preferred_base_before_suffixing() {
    let occupants = vec![OccupantSnapshot {
        full_jid: "alice@example.com/web".parse().expect("alice jid"),
        nick: "GitHub".to_string(),
        affiliation: waddle_xmpp::Affiliation::Member,
        role: waddle_xmpp::Role::Participant,
    }];

    assert_eq!(
        available_bot_nick_with_base(&occupants, "GitHub"),
        "GitHub-2"
    );
}

#[test]
fn bot_nick_sanitizes_invalid_resource_base_before_joining() {
    assert_eq!(
        available_bot_nick_with_base(&[], "GitHub\u{0}Deploys"),
        "GitHubDeploys"
    );
}
