use super::bot::{
    available_bot_nick, available_bot_nick_with_base, dispatch_bot_groupchat_response,
    BotGroupchatDispatch,
};
use super::groupchat_archive::room_scoped_reply_to_attr;
use super::groupchat_validation::lookup_groupchat_retraction_target;
use super::room_dispatch::normalize_thread_create_source;
use super::*;
use kameo::actor::Spawn;
use std::io;
use waddle_xmpp::xep::{set_thread_create, ThreadCreate};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

mod archive;
mod capture;
mod carbons;
mod enrichment;
mod groupchat_retraction;
mod groupchat_retry;
mod offline_delivery;
mod plan;
mod room_dispatch;
mod room_subject;
mod routing_detached_delivery;
mod routing_fanout_pass;
mod routing_full_jid_fallback;
mod routing_local_account;
mod routing_negative_priority;
mod routing_route_to_connection;
mod undeliverable_bounce;

#[derive(Clone)]
struct CaptureWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn test_registry() -> ConnectionRegistry {
    ConnectionRegistry::new()
}

/// Register a resource into BOTH the DashMap `ConnectionRegistry` and the
/// actor-authoritative `UserRegistryActor`, sharing the SAME `Arc`-backed
/// `ConnectionEntry` exactly as the production bind path does (ADR-0017
/// Phase 1). Bare-JID selection reads the actor tree after the Slice 1
/// cutover, so tests that drive `route_to_connection` bare-JID delivery must
/// mirror into the actor here — a later `update_presence` on the DashMap
/// mutates the shared atomics, so the actor observes the same availability.
async fn register_into_both_tiers(
    connection_registry: &ConnectionRegistry,
    user_registry: &kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    jid: &jid::FullJid,
    sender: tokio::sync::mpsc::Sender<waddle_xmpp::registry::OutboundStanza>,
) {
    connection_registry.register_with_carbons(jid.clone(), sender, false);
    let entry = connection_registry
        .get_entry(jid)
        .expect("entry just registered into the DashMap");
    let registered =
        crate::server::dual_registration::mirror_register(user_registry, jid.clone(), entry).await;
    assert!(
        registered,
        "authoritative mirror register should confirm {jid} in the actor tree"
    );
}

fn result_iq(id: &str) -> Iq {
    Iq::Result {
        from: None,
        to: None,
        id: id.to_string(),
        payload: Some(Element::builder("query", "jabber:iq:roster").build()),
    }
}

/// Parse boundary for test fixtures: literal → typed JID, once.
fn jid(value: &str) -> jid::Jid {
    value.parse().expect("valid test jid")
}

fn chat_msg(from: jid::Jid, to: jid::Jid, body: &str) -> xmpp_parsers::message::Message {
    let mut m = xmpp_parsers::message::Message::new(Some(to));
    m.from = Some(from);
    m.type_ = xmpp_parsers::message::MessageType::Chat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m
}

fn drain_inbound(
    rx: &mut tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
) -> Vec<waddle_xmpp::registry::OutboundStanza> {
    let mut out = Vec::new();
    while let Ok(stanza) = rx.try_recv() {
        out.push(stanza);
    }
    out
}

#[tokio::test]
async fn interprets_send_stanza() {
    let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(
        result_iq("x"),
    ))))];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
    assert_eq!(outcome.frames.len(), 1);
    assert!(outcome.frames[0].contains("type='result'"));
    assert!(outcome.frames[0].contains("id='x'"));
    assert!(!outcome.close);
}

#[tokio::test]
async fn interprets_close_transport() {
    let events = vec![OutboundEvent::CloseTransport];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
    assert!(outcome.close);
    assert!(outcome.frames.is_empty());
}

#[tokio::test]
async fn interprets_log_is_noop_for_caller() {
    let events = vec![OutboundEvent::Log {
        level: tracing::Level::INFO,
        message: "hello".to_string(),
    }];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
    assert!(outcome.frames.is_empty());
    assert!(!outcome.close);
}

#[tokio::test]
async fn preserves_frame_order_across_multiple_events() {
    let events = vec![
        OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(result_iq("a"))))),
        OutboundEvent::Log {
            level: tracing::Level::DEBUG,
            message: "between".to_string(),
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Iq(Box::new(result_iq("b"))))),
    ];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
    assert_eq!(outcome.frames.len(), 2);
    assert!(outcome.frames[0].contains("id='a'"));
    assert!(outcome.frames[1].contains("id='b'"));
}

#[tokio::test]
async fn send_stanza_preserves_xep_0201_thread_on_wire() {
    let mut msg = chat_msg(
        jid("alice@example.com/web"),
        jid("bob@example.com"),
        "threaded hi",
    );
    msg.thread = Some(xmpp_parsers::message::Thread {
        id: "root-thread".to_string(),
        parent: None,
    });

    let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Message(msg)))];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;

    assert_eq!(outcome.frames.len(), 1);
    assert!(
        outcome.frames[0].contains("<thread>root-thread</thread>"),
        "SendStanza must preserve RFC 6121/XEP-0201 thread on the wire: {}",
        outcome.frames[0]
    );
}

// -----------------------------------------------------------------
// -----------------------------------------------------------------
// #229 PR15 — headless offline-recipient pass
// -----------------------------------------------------------------
//
// When `RouteToConnection` lands a bare-JID at a local user with no
// available resources, the interpreter constructs a transient
// `XmppStateMachine` for the recipient (loaded blocklist), feeds
// `StanzaFromPeer`, and recursively interprets the resulting events
// with a recursion depth cap. Persists archive + inbox + incoming
// blocking; drops `RouteToConnection`/`SendStanza`/`SendCarbons`
// from the headless pass.

/// Build a `Deps` configured for offline-recipient-pass tests:
/// real dispatcher with the message handler chain registered, real
/// MAM + inbox storage, blocklist storage seeded by the caller.
fn offline_pass_deps<'a>(
    registry: &'a ConnectionRegistry,
    mam: &'a Arc<dyn MamStorage>,
    inbox: &'a Arc<dyn InboxStorage>,
    blocking: &'a Arc<dyn BlockingStorage>,
    dispatcher: &'a Arc<StanzaDispatcher>,
) -> Deps<'a> {
    Deps {
        connection_registry: registry,
        user_registry: None,
        sm_session_registry: None,
        mam_storage: Some(mam),
        inbox_storage: Some(inbox),
        extension_manager: None,
        room_registry: None,
        web_socket_state: None,
        authenticated_principal: None,
        local_domain: "example.com",
        blocking_storage: Some(blocking),
        message_dispatcher: Some(dispatcher),
        pending_delivery_storage: None,
        ordered_relay_origin: None,
        sfu: None,
        ingress_effect_capture: None,
        effects: &crate::server::routes::interpret::effects::ImmediateSink,
    }
}

/// Like [`offline_pass_deps`] but with the actor-backed `user_registry` wired,
/// for fan-out tests that register a LIVE recipient and assert live delivery —
/// bare-JID selection (ADR-0017 Slice 1) reads the actor tree, so those tests
/// must mirror the recipient into it (see `register_into_both_tiers`).
fn offline_pass_deps_with_user_registry<'a>(
    registry: &'a ConnectionRegistry,
    user_registry: &'a kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>,
    mam: &'a Arc<dyn MamStorage>,
    inbox: &'a Arc<dyn InboxStorage>,
    blocking: &'a Arc<dyn BlockingStorage>,
    dispatcher: &'a Arc<StanzaDispatcher>,
) -> Deps<'a> {
    Deps {
        user_registry: Some(user_registry),
        ..offline_pass_deps(registry, mam, inbox, blocking, dispatcher)
    }
}

fn pipelined_dispatcher() -> Arc<StanzaDispatcher> {
    let mut d = StanzaDispatcher::new();
    waddle_xmpp::protocol::handlers::register_default_message_handlers(&mut d);
    Arc::new(d)
}

fn message_thread_id(message: &Message) -> Option<String> {
    message
        .thread
        .as_ref()
        .map(|thread| thread.id.clone())
        .or_else(|| {
            extract_forum_action(message).and_then(|action| match action {
                ForumAction::Reply(reply) => Some(reply.thread_id),
                ForumAction::CreateThread(_) => message.id.as_ref().map(|id| id.0.clone()),
            })
        })
}

/// Seed the room archive with a single message whose **archive
/// primary key** (`row.id`) differs from its **wire message id**
/// (`row.stanza_id`), so a successful lookup proves which one the
/// caller is keying on.
async fn seed_groupchat_archive_row(
    mam: &Arc<dyn MamStorage>,
    room: &jid::BareJid,
    archive_pk: &str,
    wire_id: &str,
) -> MamArchivedMessage {
    let sender: jid::Jid = format!("{room}/alice").parse().expect("room/nick jid");
    let mut archived_wire_message = xmpp_parsers::message::Message::new(None);
    archived_wire_message.from = Some(sender.clone());
    archived_wire_message.id = Some(xmpp_parsers::message::Id(wire_id.to_string()));
    archived_wire_message.type_ = XmppMessageType::Groupchat;
    archived_wire_message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "remove me".to_string());
    archived_wire_message
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            archive_pk,
            &jid::Jid::from(room.clone()),
        ));
    let mut stanza_xml_bytes = Vec::new();
    Element::from(archived_wire_message)
        .write_to(&mut stanza_xml_bytes)
        .expect("serialize archived wire message");
    let row = MamArchivedMessage {
        id: archive_pk.to_string(),
        timestamp: chrono::Utc::now(),
        from: sender,
        to: jid::Jid::from(room.clone()),
        body: Some("remove me".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            wire_id,
            jid::Jid::from(room.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: XmppMessageType::Groupchat,
        stanza_xml: Some(
            String::from_utf8(stanza_xml_bytes).expect("archived wire message xml is utf-8"),
        ),
        rich: None,
        nickname_generation: Some(0),
    };
    mam.store_message(room, &row).await.expect("seed mam row");
    row
}
// ---------------------------------------------------------------------
// #1245 — full-JID DM to a detached XEP-0198 resource runs the shared
// recipient pipeline (stanza-id + archive + inbox) and queues the
// PROCESSED stanza for replay.
// ---------------------------------------------------------------------

fn detached_dm_session(
    stream_id: &str,
    jid: &jid::FullJid,
) -> waddle_xmpp::stream_management::DetachedSession {
    waddle_xmpp::stream_management::DetachedSession {
        stream_id: stream_id.to_string(),
        user_id: jid.to_bare().to_string(),
        jid: jid.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: true,
        blocklist_interested: false,
        presence_available: true,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    }
}

mod delivery_plan;
