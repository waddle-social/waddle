use super::bot::{
    available_bot_nick, available_bot_nick_with_base, dispatch_bot_groupchat_response,
    BotGroupchatDispatch,
};
use super::groupchat_archive::room_scoped_reply_to_attr;
use super::groupchat_validation::lookup_groupchat_retraction_target;
use super::room_dispatch::normalize_thread_create_source;
use super::*;
use kameo::actor::Spawn;
use waddle_xmpp::xep::{set_thread_create, ThreadCreate};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

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

// -----------------------------------------------------------------
// XEP-0280 — SendCarbons fan-out
// -----------------------------------------------------------------

fn chat_msg(from: &str, to: &str, body: &str) -> xmpp_parsers::message::Message {
    let mut m = xmpp_parsers::message::Message::new(Some(to.parse().expect("jid")));
    m.from = Some(from.parse().expect("jid"));
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
async fn xep_0280_send_carbons_fans_out_to_other_carbon_enabled_resources() {
    let registry = ConnectionRegistry::new();
    // Owner: alice. Two resources — web (originating, excluded)
    // and phone (carbon-enabled, expected target).
    let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
    let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(alice_web.clone(), _web_tx, true);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(alice_phone.clone(), phone_tx, true);

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::SendCarbons {
        owner,
        message: Box::new(original),
        kind: CarbonKind::Sent,
        exclude: vec![alice_web],
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

    // Verify the XEP-0280 <sent xmlns='urn:xmpp:carbons:2'> wrapper and
    // its nested XEP-0297 <forwarded xmlns='urn:xmpp:forward:0'> payload.
    let received = drain_inbound(&mut phone_rx);
    assert_eq!(received.len(), 1, "alice/phone received one carbon");
    let stanza = &received[0].stanza;
    let msg = match stanza {
        Stanza::Message(m) => m,
        other => panic!("expected Message stanza, got {other:?}"),
    };
    let sent = msg
        .payloads
        .iter()
        .find(|p| p.name() == "sent" && p.ns() == "urn:xmpp:carbons:2")
        .expect("carbon must carry <sent xmlns='urn:xmpp:carbons:2'/>");
    assert!(
        sent.children()
            .any(|p| p.name() == "forwarded" && p.ns() == "urn:xmpp:forward:0"),
        "carbon <sent/> must carry <forwarded xmlns='urn:xmpp:forward:0'/>"
    );
}

#[tokio::test]
async fn xep_0280_send_carbons_skips_originating_resource() {
    let registry = ConnectionRegistry::new();
    let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    let (web_tx, mut web_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(alice_web.clone(), web_tx, true);

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::SendCarbons {
        owner,
        message: Box::new(original),
        kind: CarbonKind::Sent,
        exclude: vec![alice_web],
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

    // No carbon to alice/web — it's the originating resource.
    let received = drain_inbound(&mut web_rx);
    assert!(received.is_empty(), "originating resource excluded");
}

#[tokio::test]
async fn xep_0280_send_carbons_skips_resources_without_carbons_enabled() {
    let registry = ConnectionRegistry::new();
    let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");
    let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(alice_web.clone(), _web_tx, true);
    // alice/phone has carbons DISABLED.
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(alice_phone.clone(), phone_tx, false);

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::SendCarbons {
        owner,
        message: Box::new(original),
        kind: CarbonKind::Sent,
        exclude: vec![alice_web],
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

    let received = drain_inbound(&mut phone_rx);
    assert!(received.is_empty(), "carbons-disabled resource skipped");
}

#[tokio::test]
async fn xep_0280_send_carbons_received_kind_emits_received_envelope() {
    let registry = ConnectionRegistry::new();
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let (_desk_tx, _desk_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(bob_desk.clone(), _desk_tx, true);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(bob_phone.clone(), phone_tx, true);

    let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
    let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::SendCarbons {
        owner,
        message: Box::new(original),
        kind: CarbonKind::Received,
        exclude: vec![bob_desk],
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

    let received = drain_inbound(&mut phone_rx);
    assert_eq!(received.len(), 1);
    let msg = match &received[0].stanza {
        Stanza::Message(m) => m,
        other => panic!("expected Message, got {other:?}"),
    };
    assert!(
        msg.payloads
            .iter()
            .any(|p| p.name() == "received" && p.ns() == "urn:xmpp:carbons:2"),
        "kind=Received emits <received xmlns='urn:xmpp:carbons:2'/>"
    );
}

#[tokio::test]
async fn xep_0280_send_carbons_queues_for_detached_xep_0198_resources() {
    // Regression test for the carbon-fan-out-skipping-detached-SM
    // bug: a XEP-0198-resumable session that briefly disconnected
    // must still receive its carbon copies via
    // record_stanza_for_detached_bound_resource so the queued
    // stanzas replay on resume. Without the detached pass, brief
    // disconnects silently lose carbon history.
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let registry = ConnectionRegistry::new();
    let alice_web: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    let alice_phone: jid::FullJid = "alice@example.com/phone".parse().expect("jid");

    // alice/web: live, originating resource (excluded).
    let (_web_tx, _web_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(alice_web.clone(), _web_tx, true);

    // alice/phone: detached, carbons-enabled, resumable via SM.
    let sm = Arc::new(InMemorySmSessionRegistry::new());
    let detached = DetachedSession {
        stream_id: "phone-stream-id".to_string(),
        user_id: "alice".to_string(),
        jid: alice_phone.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: true,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    sm.store_session(detached).await.expect("store session");

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let deps = Deps {
        connection_registry: &registry,
        user_registry: None,
        sm_session_registry: Some(&sm),
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: None,
        web_socket_state: None,
        authenticated_session: None,
        local_domain: "example.com",
        blocking_storage: None,
        message_dispatcher: None,
        pending_delivery_storage: None,
        ordered_relay_origin: None,
    };
    let _outcome = interpret(
        vec![OutboundEvent::SendCarbons {
            owner: owner.clone(),
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: vec![alice_web],
        }],
        &deps,
    )
    .await;

    // The detached resource should have a queued carbon ready
    // for resume — peek the session and assert a non-empty
    // outbound replay queue.
    let session = sm
        .peek_session("phone-stream-id")
        .await
        .expect("peek")
        .expect("session present");
    assert!(
        !session.unacked_stanzas.is_empty(),
        "detached SM session must have at least one queued carbon for resume"
    );
}

// -----------------------------------------------------------------
// XEP-0313 — ArchiveDirect persistence
// -----------------------------------------------------------------

#[tokio::test]
async fn xep_0313_archive_direct_persists_to_mam_storage() {
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let from: jid::BareJid = "alice@example.com".parse().expect("bare");
    let to: jid::BareJid = "bob@example.com".parse().expect("bare");
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hello");
    msg.id = Some(xmpp_parsers::message::Id("orig-1".to_string()));

    let events = vec![OutboundEvent::ArchiveDirect {
        archive_jid: archive_jid.clone(),
        from: from.into(),
        to: to.into(),
        message: Box::new(msg),
    }];
    let _outcome = interpret(events, &deps).await;

    let stored = mam
        .query_messages(
            &archive_jid,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query");
    assert_eq!(
        stored.messages.len(),
        1,
        "ArchiveDirect persists exactly one row"
    );
    let row = &stored.messages[0];
    assert_eq!(row.from.to_string(), "alice@example.com");
    assert_eq!(row.to.to_string(), "bob@example.com");
    assert_eq!(row.body.as_deref(), Some("hello"));
    assert_eq!(
        row.stanza_id.as_ref().map(|s| s.id.as_str()),
        Some("orig-1")
    );
}

#[tokio::test]
async fn direct_correction_preview_ref_target_resolves_to_wire_message_id() {
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let sender: jid::BareJid = "alice@example.com".parse().expect("bare");
    mam.store_message(
        &archive_jid,
        &ArchivedMessage {
            id: "canonical-archive-id".to_string(),
            body: Some("message with preview".to_string()),
            stanza_id: Some(StanzaId::new(
                "wire-msg-id",
                jid::Jid::from(archive_jid.clone()),
            )),
            message_type: xmpp_parsers::message::MessageType::Chat,
            ..ArchivedMessage::for_test(
                "alice@example.com/web".parse().expect("jid"),
                "bob@example.com".parse().expect("jid"),
            )
        },
    )
    .await
    .expect("seed direct archive row");

    let target = super::direct_archive::resolve_direct_correction_target_message_id(
        &deps,
        &archive_jid,
        &sender,
        "wire-msg-id",
    )
    .await;

    assert_eq!(
        target.as_deref(),
        Some("wire-msg-id"),
        "link preview refs are keyed by original direct message wire ids, not canonical MAM ids"
    );

    mam.store_message(
        &archive_jid,
        &ArchivedMessage {
            id: "origin-canonical-archive-id".to_string(),
            body: Some("message with origin id".to_string()),
            origin_id: Some(OriginId::new("client-origin-id")),
            stanza_id: Some(StanzaId::new(
                "origin-wire-msg-id",
                jid::Jid::from(archive_jid.clone()),
            )),
            message_type: xmpp_parsers::message::MessageType::Chat,
            ..ArchivedMessage::for_test(
                "alice@example.com/phone".parse().expect("jid"),
                "bob@example.com".parse().expect("jid"),
            )
        },
    )
    .await
    .expect("seed origin-id direct archive row");

    let target = super::direct_archive::resolve_direct_correction_target_message_id(
        &deps,
        &archive_jid,
        &sender,
        "client-origin-id",
    )
    .await;

    assert_eq!(
        target.as_deref(),
        Some("origin-wire-msg-id"),
        "origin-id corrections must still clear refs by the original direct wire message id"
    );
}

#[tokio::test]
async fn xep_0359_archive_ref_pivots_inbox_row_to_mam_row_via_archive_or_stanza_id() {
    // End-to-end of the bug Qodo + Codex flagged: inbox writes
    // `archive_ref` from the canonical XEP-0359 `<stanza-id>`
    // stamp, and `MamStorage::get_message_by_archive_or_stanza_id`
    // must resolve that same id against `archive_jid` by querying
    // both the archive's primary key (`id`) and the wire id
    // (`stanza_id`). If the projection ever stops using the
    // canonical stamp as `ArchivedMessage.id`, the inbox row
    // points at a dangling stanza-id and clients can't pivot to
    // the archive.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp_core::xep0359::{build_stanza_id_element, StanzaId};
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "pivot test");
    msg.id = Some(xmpp_parsers::message::Id("wire-id".to_string()));
    // Simulate CanonicalizeHandler stamping the canonical id
    // under alice's archive — the same id InboxHandler will
    // emit as `archive_ref`.
    let canonical_id = "alice-canonical-1";
    msg.payloads.push(build_stanza_id_element(
        canonical_id,
        &jid::Jid::from(alice.clone()),
    ));

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(msg.clone()),
        },
        OutboundEvent::ProjectInbox {
            owner: alice.clone(),
            peer: bob.clone(),
            message: Box::new(msg),
            archive_ref: StanzaId::new(canonical_id, jid::Jid::from(alice.clone())),
            increment_unread: false,
        },
    ];
    let _outcome = interpret(events, &deps).await;

    // Inbox row carries the canonical stamp.
    let entries = inbox_concrete.list(&alice).await.expect("inbox list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].last_stanza_id, canonical_id);

    // The same id resolves a MAM row in alice's archive — pivot
    // works. The XEP-0359 canonical stamp is stored as the row's
    // `id` (primary key) per the legacy projection shape, so the
    // pivot uses `get_message_by_archive_or_stanza_id` (queries
    // both `id` and `stanza_id`).
    let row = mam
        .get_message_by_archive_or_stanza_id(&alice, canonical_id)
        .await
        .expect("mam lookup")
        .expect("MAM row keyed by canonical stanza-id");
    assert_eq!(row.id, canonical_id);
    assert_eq!(row.body.as_deref(), Some("pivot test"));
}

#[tokio::test]
async fn origin_id_dedup_rewrites_downstream_archive_refs_to_existing_mam_id() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{
        build_origin_id_element, build_stanza_id_element, OriginId, StanzaId,
    };

    let registry = ConnectionRegistry::new();
    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let origin_id = OriginId::new("retry-origin-1");

    mam_concrete
        .store_message(
            &alice,
            &ArchivedMessage {
                id: "existing-archive-id".to_string(),
                body: Some("original copy".to_string()),
                origin_id: Some(origin_id.clone()),
                message_type: xmpp_parsers::message::MessageType::Chat,
                ..ArchivedMessage::for_test(
                    "alice@example.com/web-old".parse().expect("jid"),
                    "bob@example.com".parse().expect("jid"),
                )
            },
        )
        .await
        .expect("seed original archive row");

    let mut retry = chat_msg(
        "alice@example.com/web-new",
        "bob@example.com",
        "original copy",
    );
    retry
        .payloads
        .push(build_origin_id_element(origin_id.as_str()));
    retry.payloads.push(build_stanza_id_element(
        "fresh-retry-archive-id",
        &jid::Jid::from(alice.clone()),
    ));

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(retry.clone()),
        },
        OutboundEvent::ProjectInbox {
            owner: alice.clone(),
            peer: bob,
            message: Box::new(retry.clone()),
            archive_ref: StanzaId::new("fresh-retry-archive-id", jid::Jid::from(alice.clone())),
            increment_unread: false,
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Message(retry))),
    ];
    let outcome = interpret(events, &deps).await;

    assert_eq!(
        mam_concrete.count_messages(&alice).await.expect("count"),
        1,
        "origin-id retry must not insert a second MAM row"
    );
    let entries = inbox_concrete.list(&alice).await.expect("inbox list");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].last_stanza_id, "existing-archive-id",
        "downstream inbox projection must point at the retained MAM row"
    );
    assert_eq!(outcome.frames.len(), 1);
    assert!(
        outcome.frames[0].contains("existing-archive-id"),
        "wire stanza-id must name the retained MAM row"
    );
    assert!(
        !outcome.frames[0].contains("fresh-retry-archive-id"),
        "wire stanza-id must not point at the skipped duplicate row"
    );
}

#[tokio::test]
async fn origin_id_collision_with_distinct_content_keeps_fresh_archive_refs() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{
        build_origin_id_element, build_stanza_id_element, OriginId, StanzaId,
    };

    let registry = ConnectionRegistry::new();
    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let origin_id = OriginId::new("retry-origin-collision");

    mam_concrete
        .store_message(
            &alice,
            &ArchivedMessage {
                id: "existing-archive-id".to_string(),
                body: Some("original copy".to_string()),
                origin_id: Some(origin_id.clone()),
                message_type: xmpp_parsers::message::MessageType::Chat,
                ..ArchivedMessage::for_test(
                    "alice@example.com/web-old".parse().expect("jid"),
                    "bob@example.com".parse().expect("jid"),
                )
            },
        )
        .await
        .expect("seed original archive row");

    let mut distinct = chat_msg(
        "alice@example.com/web-new",
        "bob@example.com",
        "new content",
    );
    distinct
        .payloads
        .push(build_origin_id_element(origin_id.as_str()));
    distinct.payloads.push(build_stanza_id_element(
        "fresh-distinct-archive-id",
        &jid::Jid::from(alice.clone()),
    ));

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(distinct.clone()),
        },
        OutboundEvent::ProjectInbox {
            owner: alice.clone(),
            peer: bob,
            message: Box::new(distinct.clone()),
            archive_ref: StanzaId::new("fresh-distinct-archive-id", jid::Jid::from(alice.clone())),
            increment_unread: false,
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Message(distinct))),
    ];
    let outcome = interpret(events, &deps).await;

    assert_eq!(
        mam_concrete.count_messages(&alice).await.expect("count"),
        2,
        "origin-id reuse with distinct content must archive a new MAM row"
    );
    let entries = inbox_concrete.list(&alice).await.expect("inbox list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].last_stanza_id, "fresh-distinct-archive-id");
    assert_eq!(outcome.frames.len(), 1);
    assert!(outcome.frames[0].contains("fresh-distinct-archive-id"));
    assert!(!outcome.frames[0].contains("existing-archive-id"));
}

#[tokio::test]
async fn xep_0313_archive_direct_writes_one_entry_per_event() {
    // Sender pass + recipient pass on the same dispatch (true
    // local-to-local) emit two events with distinct archive_jids
    // — the interpreter writes one entry per archive.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(msg.clone()),
        },
        OutboundEvent::ArchiveDirect {
            archive_jid: bob.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(msg),
        },
    ];
    let _outcome = interpret(events, &deps).await;

    let alice_archive = mam
        .query_messages(
            &alice,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query alice");
    let bob_archive = mam
        .query_messages(
            &bob,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        alice_archive.messages.len(),
        1,
        "alice archive has the sender-pass entry"
    );
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "bob archive has the recipient-pass entry"
    );
}

#[tokio::test]
async fn xep_0313_archive_direct_drops_when_storage_errors() {
    // Storage errors must NOT fail dispatch. We use a fake that
    // always errors and assert interpret returns normally; the
    // archive write is logged-and-dropped.
    use async_trait::async_trait;
    use waddle_xmpp::mam::storage::{MamStorage, MamStorageError};
    use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamResult};

    struct FailingMam;
    #[async_trait]
    impl MamStorage for FailingMam {
        async fn store_message(
            &self,
            _: &jid::BareJid,
            _: &ArchivedMessage,
        ) -> Result<String, MamStorageError> {
            Err(MamStorageError::Database("simulated".into()))
        }
        async fn query_messages(
            &self,
            _: &jid::BareJid,
            _: waddle_xmpp::mam::MamArchiveKind,
            _: &MamQuery,
        ) -> Result<MamResult, MamStorageError> {
            Ok(MamResult {
                messages: Vec::new(),
                complete: true,
                first_id: None,
                last_id: None,
                count: Some(0),
            })
        }
        async fn get_message(&self, _: &str) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn replace_with_tombstone(
            &self,
            _: &str,
            _: waddle_xmpp::mam::ArchivedTombstone,
        ) -> Result<bool, MamStorageError> {
            Ok(false)
        }
        async fn get_message_by_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn get_message_by_message_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn get_message_by_sender_and_origin_id(
            &self,
            _: &jid::BareJid,
            _: waddle_xmpp::mam::MamArchiveKind,
            _: &jid::Jid,
            _: &waddle_xmpp_core::xep0359::OriginId,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn get_message_by_archive_or_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn count_messages(&self, _: &jid::BareJid) -> Result<u32, MamStorageError> {
            Ok(0)
        }
        async fn delete_before(
            &self,
            _: &jid::BareJid,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, MamStorageError> {
            Ok(0)
        }
    }

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(FailingMam);
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");
    let events = vec![OutboundEvent::ArchiveDirect {
        archive_jid: alice.clone(),
        from: alice.into(),
        to: bob.into(),
        message: Box::new(msg),
    }];
    let outcome = interpret(events, &deps).await;
    // No frames, no close — error swallowed.
    assert!(outcome.frames.is_empty());
    assert!(!outcome.close);
}

// -----------------------------------------------------------------
// Inbox projection
// -----------------------------------------------------------------

#[tokio::test]
async fn inbox_project_writes_owner_peer_keyed_row_with_typed_archive_ref() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let peer: jid::BareJid = "bob@example.com".parse().expect("bare");
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hi there");
    msg.id = Some(xmpp_parsers::message::Id("origin-X".to_string()));

    let events = vec![OutboundEvent::ProjectInbox {
        owner: owner.clone(),
        peer: peer.clone(),
        message: Box::new(msg),
        archive_ref: StanzaId::new("alice-archive-1", jid::Jid::from(owner.clone())),
        increment_unread: false,
    }];
    let _outcome = interpret(events, &deps).await;

    let entries = inbox_concrete.list(&owner).await.expect("list");
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.partner, peer);
    assert_eq!(
        entry.last_stanza_id, "alice-archive-1",
        "last_stanza_id is sourced from the typed archive_ref, not the wire id"
    );
    assert_eq!(entry.unread, 0, "increment_unread=false leaves unread at 0");
}

#[tokio::test]
async fn inbox_project_increment_unread_bumps_recipient_count() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
    let peer: jid::BareJid = "alice@example.com".parse().expect("bare");
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bob");

    let events = vec![OutboundEvent::ProjectInbox {
        owner: owner.clone(),
        peer: peer.clone(),
        message: Box::new(msg),
        archive_ref: StanzaId::new("bob-archive-1", jid::Jid::from(owner.clone())),
        increment_unread: true,
    }];
    let _outcome = interpret(events, &deps).await;

    let total = inbox_concrete.total_unread(&owner).await.expect("unread");
    assert_eq!(
        total, 1,
        "increment_unread=true bumps the owner's unread count"
    );
}

// -----------------------------------------------------------------
// XEP-0308/0424/0461 — LookupArchivedMessage callback round-trip
// -----------------------------------------------------------------

#[tokio::test]
async fn xep_0424_lookup_archived_message_by_stanza_id_feeds_archived_loaded_back() {
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    // Seed the archive with a row keyed under alice's bare,
    // canonical stamp = "canon-A1".
    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let row = ArchivedMessage {
        id: "canon-A1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: Some("hello".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "canon-A1",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: XmppMessageType::Chat,
        stanza_xml: Some(
            r#"<message xmlns='jabber:client' type='chat' from='alice@example.com/web' to='bob@example.com'><body>hello</body></message>"#.to_string(),
        ),
        rich: None,
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(7),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("canon-A1", jid::Jid::from(archive_jid.clone())),
        },
    }];
    let outcome = interpret(events, &deps).await;

    assert_eq!(outcome.feedback.len(), 1);
    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded { id, result } => {
            assert_eq!(id, CallbackId(7));
            let archived = result.expect("row resolved");
            assert_eq!(archived.stanza_id.as_str(), "canon-A1");
            assert_eq!(archived.stanza_id.by, jid::Jid::from(archive_jid.clone()));
            assert!(!archived.tombstoned);
            assert_eq!(
                archived.message.bodies.get("").cloned(),
                Some("hello".to_string()),
                "stanza_xml is parsed back into a typed Message"
            );
        }
        other => panic!("expected ArchivedMessageLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0424_lookup_archived_message_not_found_feeds_none_back() {
    use waddle_xmpp::mam::InMemoryMamStorage;
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(11),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("never-stamped", jid::Jid::from(archive_jid)),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(11),
            result: None,
        } => {}
        other => panic!("expected ArchivedMessageLoaded(None), got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_by_origin_id_feeds_archived_loaded_back() {
    // OriginId lookup MUST be sender-scoped per the typed
    // `MessageRef::OriginId { sender, origin_id }` contract.
    // Seed two rows in alice's archive that share the same
    // `origin_id` value but come from different senders:
    // post-filter on `sender` must pick the alice-authored row,
    // not the bob-authored one.
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::OriginId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let alice_bare: jid::BareJid = "alice@example.com".parse().expect("bare");

    // Bob-authored row in alice's archive (cross-resource self
    // chat / received DM) sharing the colliding origin-id.
    let bob_row = ArchivedMessage {
        id: "row-from-bob".to_string(),
        timestamp: chrono::Utc::now(),
        from: "bob@example.com".parse().expect("jid"),
        to: "alice@example.com".parse().expect("jid"),
        body: Some("from bob".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "alice-stamp-bob",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("collision")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    // Alice-authored row in alice's archive (sender-side) with
    // the same origin-id.
    let alice_row = ArchivedMessage {
        id: "row-from-alice".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: Some("from alice".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "alice-stamp-alice",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("collision")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    // Insert bob's row FIRST so a naive OR-matcher would return it.
    mam.store_message(&archive_jid, &bob_row)
        .await
        .expect("seed bob");
    mam.store_message(&archive_jid, &alice_row)
        .await
        .expect("seed alice");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(21),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::OriginId {
            sender: alice_bare.clone().into(),
            origin_id: OriginId::new("collision"),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(21),
            result: Some(archived),
        } => {
            let body = archived.message.bodies.get("").cloned().unwrap_or_default();
            assert_eq!(
                body, "from alice",
                "OriginId lookup must scope to sender; bob's row was a collision decoy"
            );
        }
        other => panic!("expected alice-authored row, got {other:?}"),
    }
}

async fn assert_origin_lookup_finds_targets_beyond_default_page(mam: Arc<dyn MamStorage>) {
    use waddle_xmpp::mam::{ArchivedMessage, MamArchiveKind};
    use waddle_xmpp_core::xep0359::OriginId;

    struct SeedRow<'a> {
        id: String,
        offset: i64,
        from: &'a str,
        to: &'a str,
        origin_id: String,
        body: String,
    }

    async fn store_row(mam: &dyn MamStorage, archive: &jid::BareJid, seed: SeedRow<'_>) {
        let row = ArchivedMessage {
            id: seed.id,
            timestamp: chrono::DateTime::from_timestamp(1_700_000_000 + seed.offset, 0)
                .expect("fixture timestamp"),
            from: seed.from.parse().expect("from JID"),
            to: seed.to.parse().expect("to JID"),
            body: Some(seed.body),
            origin_id: Some(OriginId::new(seed.origin_id)),
            ..ArchivedMessage::for_test(
                seed.from.parse().expect("from JID"),
                seed.to.parse().expect("to JID"),
            )
        };
        mam.store_message(archive, &row)
            .await
            .expect("seed MAM row");
    }

    let registry = ConnectionRegistry::new();
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    // Place a cross-sender collision and enough ordinary rows ahead of the
    // target to prove lookup is sender-indexed rather than bounded by the
    // default MAM page.
    let owner_archive: jid::BareJid = "owner@example.com".parse().expect("owner archive");
    store_row(
        mam.as_ref(),
        &owner_archive,
        SeedRow {
            id: "owner-000".to_string(),
            offset: 0,
            from: "mallory@example.com",
            to: "owner@example.com",
            origin_id: "owner-target".to_string(),
            body: "collision decoy".to_string(),
        },
    )
    .await;
    for index in 1..=100 {
        store_row(
            mam.as_ref(),
            &owner_archive,
            SeedRow {
                id: format!("owner-{index:03}"),
                offset: index,
                from: "owner@example.com",
                to: "peer@example.com",
                origin_id: format!("owner-decoy-{index}"),
                body: format!("owner decoy {index}"),
            },
        )
        .await;
    }
    store_row(
        mam.as_ref(),
        &owner_archive,
        SeedRow {
            id: "owner-101".to_string(),
            offset: 101,
            from: "owner@example.com",
            to: "peer@example.com",
            origin_id: "owner-target".to_string(),
            body: "owner page two target".to_string(),
        },
    )
    .await;

    let owner_result = super::archive_lookup::lookup_archived_message(
        &deps,
        &owner_archive,
        MamArchiveKind::Personal,
        &MessageRef::OriginId {
            sender: owner_archive.clone().into(),
            origin_id: OriginId::new("owner-target"),
        },
    )
    .await
    .expect("owner target beyond default page boundary");
    assert_eq!(
        owner_result.message.bodies.get("").map(String::as_str),
        Some("owner page two target")
    );

    // Recipient-side decoys with the same origin id must not hide a later row
    // actually authored by the requested sender.
    let peer_archive: jid::BareJid = "archive@example.com".parse().expect("peer archive");
    for index in 0..=100 {
        store_row(
            mam.as_ref(),
            &peer_archive,
            SeedRow {
                id: format!("peer-{index:03}"),
                offset: index,
                from: "archive@example.com",
                to: "sender@example.com",
                origin_id: if index == 0 {
                    "peer-target".to_string()
                } else {
                    format!("peer-decoy-{index}")
                },
                body: format!("peer decoy {index}"),
            },
        )
        .await;
    }
    store_row(
        mam.as_ref(),
        &peer_archive,
        SeedRow {
            id: "peer-101".to_string(),
            offset: 101,
            from: "sender@example.com/mobile",
            to: "archive@example.com",
            origin_id: "peer-target".to_string(),
            body: "sender page two target".to_string(),
        },
    )
    .await;
    let sender: jid::BareJid = "sender@example.com".parse().expect("sender");
    let peer_result = super::archive_lookup::lookup_archived_message(
        &deps,
        &peer_archive,
        MamArchiveKind::Personal,
        &MessageRef::OriginId {
            sender: sender.into(),
            origin_id: OriginId::new("peer-target"),
        },
    )
    .await
    .expect("sender target beyond default page boundary");
    assert_eq!(
        peer_result.message.bodies.get("").map(String::as_str),
        Some("sender page two target")
    );
}

#[tokio::test]
async fn origin_lookup_finds_targets_beyond_default_page_in_memory() {
    assert_origin_lookup_finds_targets_beyond_default_page(Arc::new(
        waddle_xmpp::mam::InMemoryMamStorage::new(),
    ))
    .await;
}

#[tokio::test]
async fn origin_lookup_finds_targets_beyond_default_page_in_sqlite() {
    let storage = waddle_xmpp::mam::SqlxMamStorage::open_in_memory()
        .await
        .expect("SQLite MAM storage");
    assert_origin_lookup_finds_targets_beyond_default_page(Arc::new(storage)).await;
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_propagates_room_archive_kind() {
    use async_trait::async_trait;
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedTombstone, MamArchiveKind, MamQuery, MamResult, MamStorageError,
    };
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::OriginId;

    struct KindProbeMam {
        row: ArchivedMessage,
    }

    #[async_trait]
    impl MamStorage for KindProbeMam {
        async fn store_message(
            &self,
            _: &jid::BareJid,
            _: &ArchivedMessage,
        ) -> Result<String, MamStorageError> {
            unreachable!("lookup regression never stores")
        }

        async fn query_messages(
            &self,
            _: &jid::BareJid,
            archive_kind: MamArchiveKind,
            _: &MamQuery,
        ) -> Result<MamResult, MamStorageError> {
            assert_eq!(
                archive_kind,
                MamArchiveKind::Room,
                "event archive kind must reach MamStorage"
            );
            Ok(MamResult {
                messages: vec![self.row.clone()],
                complete: true,
                first_id: Some(self.row.id.clone()),
                last_id: Some(self.row.id.clone()),
                count: Some(1),
            })
        }

        async fn get_message(&self, _: &str) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn replace_with_tombstone(
            &self,
            _: &str,
            _: ArchivedTombstone,
        ) -> Result<bool, MamStorageError> {
            Ok(false)
        }

        async fn get_message_by_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn get_message_by_message_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn get_message_by_sender_and_origin_id(
            &self,
            _: &jid::BareJid,
            archive_kind: MamArchiveKind,
            _: &jid::Jid,
            _: &OriginId,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            assert_eq!(
                archive_kind,
                MamArchiveKind::Room,
                "event archive kind must reach MamStorage"
            );
            Ok(Some(self.row.clone()))
        }

        async fn get_message_by_archive_or_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn count_messages(&self, _: &jid::BareJid) -> Result<u32, MamStorageError> {
            Ok(0)
        }

        async fn delete_before(
            &self,
            _: &jid::BareJid,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, MamStorageError> {
            Ok(0)
        }
    }

    let registry = ConnectionRegistry::new();
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());

    let room: jid::BareJid = "room@conference.example".parse().expect("room bare");
    let row = ArchivedMessage {
        id: "room-row-1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "room@conference.example/alice"
            .parse()
            .expect("occupant jid"),
        to: "bob@example.com".parse().expect("recipient jid"),
        body: Some("room message".to_string()),
        stanza_id: None,
        thread: None,
        reply: None,
        origin_id: Some(OriginId::new("room-origin-1")),
        message_type: XmppMessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    let mam: Arc<dyn MamStorage> = Arc::new(KindProbeMam { row });
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let outcome = interpret(
        vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(22),
            archive: room.clone(),
            archive_kind: MamArchiveKind::Room,
            reference: MessageRef::OriginId {
                sender: "room@conference.example/alice"
                    .parse()
                    .expect("occupant JID"),
                origin_id: OriginId::new("room-origin-1"),
            },
        }],
        &deps,
    )
    .await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(22),
            result: Some(archived),
        } => assert_eq!(
            archived.message.bodies.get("").map(String::as_str),
            Some("room message"),
        ),
        other => panic!("room archive kind must reach storage lookup, got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_by_origin_id_rejects_cross_sender_collision() {
    // Same archive, same origin_id, different sender than
    // requested -> result MUST be None (handler will treat as
    // <item-not-found>).
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::OriginId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let row = ArchivedMessage {
        id: "row-1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "bob@example.com".parse().expect("jid"),
        to: "alice@example.com".parse().expect("jid"),
        body: Some("bob's".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "alice-stamp",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("oid-1")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    // Look up for a DIFFERENT sender (charlie) with the colliding
    // origin-id. Must surface as not-found.
    let charlie_bare: jid::BareJid = "charlie@example.com".parse().expect("bare");
    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(31),
        archive: archive_jid,
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::OriginId {
            sender: charlie_bare.into(),
            origin_id: OriginId::new("oid-1"),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(31),
            result: None,
        } => {}
        other => {
            panic!("OriginId lookup must reject cross-sender collisions, got {other:?}")
        }
    }
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_strict_stanza_id_ignores_origin_id_collision() {
    // StanzaId path uses `get_message_by_message_id` (stanza_id
    // ONLY), so a row whose `origin_id` happens to equal the
    // requested stanza-id MUST NOT be returned.
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    // Row whose origin_id equals the value the caller is looking
    // up via StanzaId. Stanza_id is something else.
    let collision_row = ArchivedMessage {
        id: "row-collide".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: Some("collide".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "real-stamp",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("queried-id")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &collision_row)
        .await
        .expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(41),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("queried-id", jid::Jid::from(archive_jid)),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(41),
            result: None,
        } => {}
        other => {
            panic!("strict stanza-id lookup must ignore origin-id collisions, got {other:?}")
        }
    }
}

#[tokio::test]
async fn xep_0424_lookup_archived_message_propagates_tombstone_state() {
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
        InMemoryMamStorage, RichMessageId,
    };
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let row = ArchivedMessage {
        id: "tomb-1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: None,
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "tomb-1",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: Some(ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                retraction_id: Some(RichMessageId::new("retract-1").expect("rich id")),
                stamp: chrono::Utc::now(),
                moderation: None,
            })),
            reply: None,
            references: Vec::new(),
            mentions: Vec::new(),
            occupant_id: None,
            muc_sender: None,
        }),
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(13),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("tomb-1", jid::Jid::from(archive_jid)),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            result: Some(archived),
            ..
        } => {
            assert!(
                archived.tombstoned,
                "ArchivedRichPayload::Tombstone surfaces as `tombstoned: true`"
            );
        }
        other => panic!("expected Some archived row, got {other:?}"),
    }
}

// -----------------------------------------------------------------
// XEP-0372 — RequestEnrichment callback round-trip
// -----------------------------------------------------------------

#[test]
fn extension_waddle_scope_matches_managed_room_context() {
    let managed_room: BareJid = "general@muc.example.com".parse().expect("room jid");
    assert_eq!(waddle_id_for_room_jid(&managed_room).as_str(), "space");

    let unmanaged_room: BareJid = "conference.example.com".parse().expect("room jid");
    assert_eq!(waddle_id_for_room_jid(&unmanaged_room).as_str(), "default");
}

#[tokio::test]
async fn enrichment_request_without_extension_manager_fails_open_with_original_message() {
    // No extension manager in Deps -> the original typed message
    // is returned unchanged via EnrichmentComplete. This is the
    // legacy fail-open contract (see `enrich_message` in the
    // legacy `message.rs` path).
    use waddle_xmpp::protocol::event::CallbackId;
    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);

    let mut original = chat_msg("alice@example.com/web", "bob@example.com", "look https://x");
    original.id = Some(xmpp_parsers::message::Id("orig-id".to_string()));

    let events = vec![OutboundEvent::RequestEnrichment {
        id: CallbackId(42),
        message: Box::new(original.clone()),
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::EnrichmentComplete {
            id: CallbackId(42),
            message,
        } => {
            assert_eq!(message.id, original.id);
            assert_eq!(
                message.bodies.get("").cloned(),
                Some("look https://x".to_string()),
            );
        }
        other => panic!("expected EnrichmentComplete, got {other:?}"),
    }
}

#[tokio::test]
async fn enrichment_failure_fail_open_feeds_original_message_back() {
    // Fail-open contract: when the extension manager has no
    // working actors (e.g. all extension RPCs failed at startup,
    // or the deployment intentionally disabled extensions),
    // `enrich_message` is a no-op and the dispatch must still
    // resume with the *original* message via EnrichmentComplete
    // — never block on enrichment, never drop the message.
    // We model this with a disabled config (no actors loaded),
    // which is the exact failure mode legacy `message.rs` falls
    // back to when the wasm runtime can't start any extension.
    use waddle_extensions::{ExtensionConfig, ExtensionManager};
    use waddle_xmpp::protocol::event::CallbackId;

    let registry = ConnectionRegistry::new();
    let em = Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            ..Default::default()
        })
        .await
        .expect("disabled extension manager"),
    );
    let deps = Deps::test_with_extension_manager(&registry, &em);

    let mut original = chat_msg(
        "alice@example.com/web",
        "bob@example.com",
        "check https://example.com",
    );
    original.id = Some(xmpp_parsers::message::Id("fail-open-id".to_string()));
    let original_payload_count = original.payloads.len();

    let events = vec![OutboundEvent::RequestEnrichment {
        id: CallbackId(123),
        message: Box::new(original.clone()),
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::EnrichmentComplete {
            id: CallbackId(123),
            message,
        } => {
            assert_eq!(
                message.id.as_ref().map(|id| id.0.as_str()),
                Some("fail-open-id"),
                "fail-open path returns the original message id"
            );
            assert_eq!(
                message.bodies.get("").cloned(),
                original.bodies.get("").cloned(),
                "fail-open path returns the original body unchanged"
            );
            assert_eq!(
                message.payloads.len(),
                original_payload_count,
                "fail-open path adds no payloads when no actor produces enrichment"
            );
        }
        other => panic!("expected EnrichmentComplete, got {other:?}"),
    }
}

#[tokio::test]
async fn enrichment_request_calls_extension_manager_and_feeds_complete_back() {
    // Wire a real (empty) ExtensionManager — no extension actors
    // configured, so `enrich_message` returns 0 enrichments and
    // we still feed back the original message via
    // EnrichmentComplete with the original CallbackId. This proves
    // the callback round-trip without depending on a live wasm
    // runtime.
    use waddle_extensions::{ExtensionConfig, ExtensionManager};
    use waddle_xmpp::protocol::event::CallbackId;

    let registry = ConnectionRegistry::new();
    let em = Arc::new(
        ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            ..Default::default()
        })
        .await
        .expect("disabled extension manager"),
    );
    let deps = Deps::test_with_extension_manager(&registry, &em);

    let mut original = chat_msg("alice@example.com/web", "bob@example.com", "ping");
    original.id = Some(xmpp_parsers::message::Id("e-id".to_string()));

    let events = vec![OutboundEvent::RequestEnrichment {
        id: CallbackId(99),
        message: Box::new(original),
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::EnrichmentComplete {
            id: CallbackId(99),
            message,
        } => {
            assert_eq!(message.id.as_ref().map(|id| id.0.as_str()), Some("e-id"));
        }
        other => panic!("expected EnrichmentComplete, got {other:?}"),
    }
}

// -----------------------------------------------------------------
// #229 PR12 — RouteToConnection delivers as PeerStanza
// -----------------------------------------------------------------

#[tokio::test]
async fn route_to_connection_full_jid_queues_peer_stanza_kind() {
    // Locks in the staged-cutover contract: full-JID
    // RouteToConnection events queue an OutboundStanza tagged
    // PeerStanza so the destination's main loop runs the
    // recipient pass before any wire write.
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    // ADR-0017 Slice 3: full-JID delivery now routes exclusively through the
    // authoritative actor (`deliver_peer_to_full` no longer has a DashMap
    // path), so register into both tiers and drive it with a `Some`
    // user_registry.
    register_into_both_tiers(&registry, &user_registry, &bob, bob_tx).await;

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    let queued = drain_inbound(&mut bob_rx);
    assert_eq!(queued.len(), 1, "delivered to bob's queue exactly once");
    assert_eq!(
        queued[0].kind,
        DeliveryKind::PeerStanza,
        "RouteToConnection MUST tag PeerStanza so the destination main \
         loop runs the recipient pass; got {:?}",
        queued[0].kind
    );
}

fn jingle_payload_for_route_test(action: &str, sid: &str) -> Element {
    Element::builder("jingle", waddle_xmpp::xep::xep0166::NS_JINGLE)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), action)
        .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid)
        .build()
}

fn call_iq_set_for_route_test(id: &str, from: &jid::FullJid, to: &jid::FullJid) -> Iq {
    Iq::Set {
        from: Some(jid::Jid::from(from.clone())),
        to: Some(jid::Jid::from(to.clone())),
        id: id.to_string(),
        payload: jingle_payload_for_route_test("session-info", "offline-sid"),
    }
}

#[tokio::test]
async fn route_to_connection_offline_full_jid_call_iq_returns_service_unavailable() {
    use waddle_xmpp::registry::UserRegistryActor;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(call_iq_set_for_route_test(
            "call-offline-1",
            &alice,
            &bob,
        )))),
    }];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "offline full-JID request IQ should produce one error frame: {:?}",
        outcome.frames
    );
    let element = Element::from_str(&outcome.frames[0]).expect("parseable IQ error");
    let iq = Iq::try_from(element).expect("typed IQ");
    let Iq::Error {
        from,
        to,
        id,
        error,
        payload,
    } = iq
    else {
        panic!("expected IQ error, got {iq:?}");
    };
    assert_eq!(id, "call-offline-1");
    assert_eq!(from, Some(jid::Jid::from(bob)));
    assert_eq!(to, Some(jid::Jid::from(alice)));
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    // RFC 6120 §8.3.1: the error echoes the original request payload so
    // the sender can correlate which stanza failed.
    let echoed = payload.expect("service-unavailable echoes the original payload");
    assert_eq!(echoed.name(), "jingle");
    assert_eq!(echoed.attr("sid"), Some("offline-sid"));
}

#[tokio::test]
async fn route_to_connection_offline_full_jid_session_terminate_is_acked() {
    // #1130 + #1131 interaction: a session-terminate forwarded to a peer
    // whose resource is already gone is a *successful* hangup — the caller
    // must get an empty <iq type='result'/> ack, never <service-unavailable/>.
    use waddle_xmpp::registry::UserRegistryActor;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let terminate = Iq::Set {
        from: Some(jid::Jid::from(alice.clone())),
        to: Some(jid::Jid::from(bob.clone())),
        id: "term-offline-1".to_string(),
        payload: jingle_payload_for_route_test("session-terminate", "offline-sid"),
    };
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Iq(Box::new(terminate))),
    }];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "an undeliverable session-terminate should be acked, not dropped: {:?}",
        outcome.frames
    );
    let iq = Iq::try_from(Element::from_str(&outcome.frames[0]).expect("parseable IQ"))
        .expect("typed IQ");
    let Iq::Result {
        from,
        to,
        id,
        payload,
    } = iq
    else {
        panic!("expected empty IQ result ack, got {iq:?}");
    };
    assert_eq!(id, "term-offline-1");
    assert_eq!(from, Some(jid::Jid::from(bob)));
    assert_eq!(to, Some(jid::Jid::from(alice)));
    assert!(payload.is_none(), "terminate ack carries no payload");
}

#[tokio::test]
async fn route_to_connection_offline_full_jid_call_iq_result_error_do_not_bounce() {
    use waddle_xmpp::registry::UserRegistryActor;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/phone".parse().expect("bob jid");
    let result_iq = Iq::Result {
        from: Some(jid::Jid::from(alice.clone())),
        to: Some(jid::Jid::from(bob.clone())),
        id: "call-result-1".to_string(),
        payload: None,
    };
    let error_iq = Iq::Error {
        from: Some(jid::Jid::from(alice)),
        to: Some(jid::Jid::from(bob.clone())),
        id: "call-error-1".to_string(),
        error: StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::NotAllowed,
            "en",
            "already failed",
        ),
        payload: None,
    };
    let events = vec![
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob.clone()),
            stanza: Box::new(Stanza::Iq(Box::new(result_iq))),
        },
        OutboundEvent::RouteToConnection {
            jid: jid::Jid::from(bob),
            stanza: Box::new(Stanza::Iq(Box::new(error_iq))),
        },
    ];

    let outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        outcome.frames.is_empty(),
        "IQ result/error stanzas must not receive synthesized service-unavailable bounces: {:?}",
        outcome.frames
    );
}

#[tokio::test]
async fn route_to_connection_bare_jid_selects_highest_priority_available_resources() {
    // RFC 6121 §8.5.2.1 resource selection: deliver to every
    // resource tied at the highest available priority. A
    // bare-JID `to` from the sender pass (handlers/route.rs
    // emits `message.to` verbatim) lands here; without selection
    // the cutover would silently drop bare-targeted 1:1 traffic.
    use waddle_xmpp::registry::{DeliveryKind, UserRegistryActor};
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let bob_tablet: jid::FullJid = "bob@example.com/tablet".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    let (tablet_tx, mut tablet_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_phone, phone_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_tablet, tablet_tx).await;
    // desk + phone available at priority 5 (tied); tablet at
    // lower priority 1. Tablet must NOT receive. `update_presence`
    // mutates the shared `Arc` atomics, so the actor sees these too.
    registry.update_presence(&bob_desk, true, 5);
    registry.update_presence(&bob_phone, true, 5);
    registry.update_presence(&bob_tablet, true, 1);

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    let desk_q = drain_inbound(&mut desk_rx);
    let phone_q = drain_inbound(&mut phone_rx);
    let tablet_q = drain_inbound(&mut tablet_rx);
    assert_eq!(
        desk_q.len(),
        1,
        "desk (tied at max priority) gets the message"
    );
    assert_eq!(
        phone_q.len(),
        1,
        "phone (tied at max priority) gets the message"
    );
    assert!(
        tablet_q.is_empty(),
        "tablet (lower priority) is excluded by RFC 6121 §8.5.2.1.2"
    );
    for q in [&desk_q, &phone_q] {
        assert_eq!(q[0].kind, DeliveryKind::PeerStanza);
    }
}

#[tokio::test]
async fn route_to_connection_bare_jid_falls_back_to_connected_resources_without_presence() {
    // RFC 6121 §8.5.2.1.1 prefers presence-available resources
    // for bare-JID delivery, but Waddle falls back to *any*
    // connected resource when no resource has emitted
    // `<presence/>` yet (matching legacy `handle_message`
    // behaviour and unblocking integration tests where clients
    // bind without sending presence). This test pins that
    // fall-back: a bare-JID DM addressed to a user with one
    // registered-but-not-presence-available resource is delivered
    // to that resource instead of falling through to the offline
    // headless pass.
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    // Registered but presence NOT made available — legacy
    // routing still delivers to this resource (tier-2 `GetResources`
    // fallback, read from the same authoritative actor).

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    let delivered = drain_inbound(&mut desk_rx);
    assert_eq!(
        delivered.len(),
        1,
        "no presence -> still delivered to connected resource as a legacy fallback"
    );
}

/// Slice 1 cutover proof: the candidate set is sourced from the
/// actor-authoritative `UserActor`, NOT the DashMap. A resource present +
/// presence-available in the DashMap but never mirrored into the
/// `UserRegistryActor` (so `GetUser` returns `Ok(None)`) must NOT be selected —
/// if selection still read the DashMap this would deliver. Pins that the actor
/// gates the candidate set.
#[tokio::test]
async fn route_to_connection_bare_jid_ignores_resource_absent_from_actor() {
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    // DashMap ONLY — never mirrored into the actor tree.
    registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
    registry.update_presence(&bob_desk, true, 5);

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "a DashMap-only resource (absent from the actor) must NOT be selected; \
         the actor is the authoritative candidate source"
    );
}

/// ADR-0017 Phase 3 Slice 9 — the fifth unit test the DashMap-selection
/// retirement was deferred behind (this replaces the Slice-1 "filter drops the
/// stale extra" guard). With the transitional DashMap-liveness intersection
/// filter retired, a *sole stale extra* — a resource still present in the actor
/// whose underlying channel has already closed at teardown — is no longer
/// filtered out of selection. It SELF-HEALS instead: the bare-JID delivery
/// selects it, the actor's `TrySend*` hits `DroppedClosed`, `try_deliver`
/// evicts it from the actor, and the message is NOT lost — the shared fan-out
/// recipient pass persists it to the recipient's MAM independently of the
/// live-send outcome, and the recipient catches up via MAM. This is exactly
/// the "self-healing via `TrySendPeer` → `DroppedClosed` eviction" the Phase 1
/// completion note deferred to this slice.
#[tokio::test]
async fn route_to_connection_bare_jid_sole_stale_extra_self_heals_via_dropped_closed() {
    use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 5);
    // Real teardown closes the resource's channel (the connection task's
    // receiver drops). The actor still holds the entry in the brief
    // lagging-unregister window — this is the exact "stale extra" the retired
    // filter used to pre-empt; now it self-heals at delivery time. Presence
    // stays "available" in the actor (teardown does not flip the atomic), so
    // selection still picks it.
    drop(desk_rx);

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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "no-loss");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _ = interpret(events, &deps).await;

    // No message loss: the recipient pass persisted the DM under bob's MAM even
    // though the sole live target's channel was dead.
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    let archive = mam
        .query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        archive.messages.len(),
        1,
        "the DM must be persisted to the recipient's MAM (no loss) despite the \
         sole selected resource having a dead channel"
    );
    assert_eq!(archive.messages[0].body.as_deref(), Some("no-loss"));

    // Self-heal: the dead resource was evicted from the actor on the failed
    // send (DroppedClosed), so a subsequent selection sees no stale extra.
    let remaining = waddle_xmpp::registry::get_resources_for_user(&user_registry, &bob_bare).await;
    assert!(
        remaining.is_empty(),
        "the stale extra must be evicted from the actor by the DroppedClosed \
         eviction, not linger (self-healing replaces the retired filter)"
    );
}

/// ADR-0017 Phase 3 Slice 9: with the Slice-1 liveness filter retired, a stale
/// extra holding a UNIQUE top priority whose channel has closed is still
/// SELECTED (the actor ranks it top — there is no DashMap intersection to drop
/// it before the max-priority collapse), but its dead channel self-heals: the
/// `DroppedClosed` send evicts it, and the message is persisted (no loss) via
/// the recipient pass. Because selection collapsed to the ghost's priority tie
/// set, the live lower-priority resources are NOT live-delivered on that first
/// attempt (they catch up via MAM) — this is the intended, accepted behaviour
/// change from retiring the exact-parity filter, NOT a regression. On the NEXT
/// bare-JID delivery — after the ghost has been evicted — routing correctly
/// reaches the true live top-priority resource, proving convergence.
#[tokio::test]
async fn route_to_connection_bare_jid_stale_top_priority_extra_self_heals_to_live_lower() {
    use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::registry::UserRegistryActor;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_mid: jid::FullJid = "bob@example.com/mid".parse().expect("jid");
    let bob_low: jid::FullJid = "bob@example.com/low".parse().expect("jid");
    let bob_stale: jid::FullJid = "bob@example.com/stale".parse().expect("jid");
    let (mid_tx, mut mid_rx) = tokio::sync::mpsc::channel(8);
    let (low_tx, mut low_rx) = tokio::sync::mpsc::channel(8);
    let (stale_tx, stale_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_mid, mid_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_low, low_tx).await;
    register_into_both_tiers(&registry, &user_registry, &bob_stale, stale_tx).await;
    registry.update_presence(&bob_mid, true, 3);
    registry.update_presence(&bob_low, true, 1);
    registry.update_presence(&bob_stale, true, 5);
    // Real teardown of the top-priority resource: its channel closes, but the
    // actor still holds the entry (presence atomic not flipped) in the
    // lagging-unregister window.
    drop(stale_rx);

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

    // First delivery: the ghost (pri 5) dominates selection; its dead channel
    // self-heals (DroppedClosed → evicted) and the message is persisted, but
    // the live pri-3/pri-1 resources are NOT live-delivered this round.
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(chat_msg(
            "alice@example.com/web",
            "bob@example.com",
            "first",
        ))),
    }];
    let _ = interpret(events, &deps).await;

    assert!(
        drain_inbound(&mut mid_rx).is_empty(),
        "while the top-priority ghost dominates selection, the live pri-3 \
         resource is not live-delivered on the first attempt (catches up via MAM)"
    );
    assert!(drain_inbound(&mut low_rx).is_empty());
    let bob_bare: jid::BareJid = "bob@example.com".parse().expect("bare");
    assert_eq!(
        mam.query_messages(
            &bob_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default()
        )
        .await
        .expect("query bob")
        .messages
        .len(),
        1,
        "no message loss: the first DM is persisted to the recipient's MAM"
    );
    // The ghost was evicted by the DroppedClosed send, leaving the two live
    // resources.
    let mut remaining =
        waddle_xmpp::registry::get_resources_for_user(&user_registry, &bob_bare).await;
    remaining.sort_by_key(|j| j.to_string());
    assert_eq!(
        remaining,
        vec![bob_low.clone(), bob_mid.clone()],
        "the stale top-priority extra must be evicted (self-heal), leaving the \
         two live resources"
    );

    // Second delivery: with the ghost gone, selection now reaches the true live
    // top-priority resource (pri 3) — convergence, no filter required.
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(chat_msg(
            "alice@example.com/web",
            "bob@example.com",
            "second",
        ))),
    }];
    let _ = interpret(events, &deps).await;

    assert_eq!(
        drain_inbound(&mut mid_rx).len(),
        1,
        "after the ghost is evicted, the true live top-priority resource (pri 3) \
         receives the next bare-JID delivery live"
    );
    assert!(
        drain_inbound(&mut low_rx).is_empty(),
        "the live pri-1 resource is still not a top-priority destination"
    );
}

/// Slice 1 degradation path: when the `UserRegistryActor` is dead (crashed /
/// poisoned), `GetUser` errors and selection degrades to empty — the caller
/// runs the offline/headless pass rather than delivering live. No live
/// delivery reaches any DashMap resource.
#[tokio::test]
async fn route_to_connection_bare_jid_degrades_to_offline_on_dead_user_registry() {
    use waddle_xmpp::registry::UserRegistryActor;
    let registry = ConnectionRegistry::new();
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    register_into_both_tiers(&registry, &user_registry, &bob_desk, desk_tx).await;
    registry.update_presence(&bob_desk, true, 5);
    // Kill the registry actor so the GetUser ask errors.
    user_registry.kill();
    tokio::task::yield_now().await;

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(
        events,
        &Deps::registry_with_user_registry(&registry, &user_registry),
    )
    .await;

    assert!(
        drain_inbound(&mut desk_rx).is_empty(),
        "a dead UserRegistryActor must degrade selection to offline, not \
         deliver live"
    );
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
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "threaded hi");
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
        authenticated_session: None,
        local_domain: "example.com",
        blocking_storage: Some(blocking),
        message_dispatcher: Some(dispatcher),
        pending_delivery_storage: None,
        ordered_relay_origin: None,
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
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hello bob");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "inbox row?");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "guard");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "drop wire");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "blocked");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "fail-closed");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@other.example.com", "remote");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@other.example.com"
            .parse::<jid::Jid>()
            .expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
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

// -----------------------------------------------------------------
// XEP-0045 §8.1 — PersistRoomSubject interpreter arm
// -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum SubjectMutationStoreMode {
    Succeed = 0,
    NotOwner = 1,
    OwnershipUnavailable = 2,
    PersistFailed = 3,
    OwnershipLostDuringPersist = 4,
}

struct SubjectMutationStore {
    mode: std::sync::atomic::AtomicU8,
    claim_store: Arc<waddle_xmpp::ownership::InProcessClaimStore>,
}

impl SubjectMutationStore {
    fn new(claim_store: Arc<waddle_xmpp::ownership::InProcessClaimStore>) -> Self {
        Self {
            mode: std::sync::atomic::AtomicU8::new(SubjectMutationStoreMode::Succeed as u8),
            claim_store,
        }
    }

    fn set_mode(&self, mode: SubjectMutationStoreMode) {
        self.mode
            .store(mode as u8, std::sync::atomic::Ordering::SeqCst);
    }

    fn mode(&self) -> SubjectMutationStoreMode {
        match self.mode.load(std::sync::atomic::Ordering::SeqCst) {
            value if value == SubjectMutationStoreMode::Succeed as u8 => {
                SubjectMutationStoreMode::Succeed
            }
            value if value == SubjectMutationStoreMode::NotOwner as u8 => {
                SubjectMutationStoreMode::NotOwner
            }
            value if value == SubjectMutationStoreMode::OwnershipUnavailable as u8 => {
                SubjectMutationStoreMode::OwnershipUnavailable
            }
            value if value == SubjectMutationStoreMode::PersistFailed as u8 => {
                SubjectMutationStoreMode::PersistFailed
            }
            value if value == SubjectMutationStoreMode::OwnershipLostDuringPersist as u8 => {
                SubjectMutationStoreMode::OwnershipLostDuringPersist
            }
            value => panic!("invalid subject mutation store mode: {value}"),
        }
    }

    async fn exact_fence_matches(
        &self,
        room_jid: &jid::BareJid,
        fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> Result<bool, waddle_xmpp::XmppError> {
        use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType};

        let expected_entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        if fence.entity != expected_entity {
            return Ok(false);
        }
        self.claim_store
            .fence(&fence.entity, &fence.owner, fence.epoch)
            .await
            .map_err(|error| waddle_xmpp::XmppError::internal(error.to_string()))
    }
}

impl waddle_xmpp::muc::MucDurableStore for SubjectMutationStore {
    fn load_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(None)
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn save_config_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        _waddle_id: &'a str,
        _channel_id: &'a str,
        _config: &'a waddle_xmpp::muc::RoomConfig,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn save_subject_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        let mode = self.mode();
        Box::pin(async move {
            if !self.exact_fence_matches(room_jid, fence).await? {
                return Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                });
            }
            match mode {
                SubjectMutationStoreMode::PersistFailed => Err(waddle_xmpp::XmppError::internal(
                    "subject persist failed in interpreter test",
                )),
                SubjectMutationStoreMode::OwnershipLostDuringPersist => {
                    use waddle_xmpp::ownership::ClaimStore;

                    self.claim_store
                        .release_exact(&fence.entity, &fence.owner, fence.epoch)
                        .await
                        .map_err(|error| waddle_xmpp::XmppError::internal(error.to_string()))?;
                    Err(waddle_xmpp::XmppError::OwnershipLost {
                        entity: fence.entity.clone(),
                    })
                }
                _ => Ok(()),
            }
        })
    }

    fn save_affiliation_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn delete_room_state_fenced<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
        Box::pin(async move {
            if self.exact_fence_matches(room_jid, fence).await? {
                Ok(())
            } else {
                Err(waddle_xmpp::XmppError::OwnershipLost {
                    entity: fence.entity.clone(),
                })
            }
        })
    }

    fn check_exact_claim_fence<'a>(
        &'a self,
        room_jid: &'a jid::BareJid,
        fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> waddle_xmpp::muc::MucDurableFuture<'a, bool> {
        let mode = self.mode();
        Box::pin(async move {
            match mode {
                SubjectMutationStoreMode::Succeed
                | SubjectMutationStoreMode::PersistFailed
                | SubjectMutationStoreMode::OwnershipLostDuringPersist => {
                    self.exact_fence_matches(room_jid, fence).await
                }
                SubjectMutationStoreMode::NotOwner => Ok(false),
                SubjectMutationStoreMode::OwnershipUnavailable => Err(
                    waddle_xmpp::XmppError::internal("ownership probe failed in interpreter test"),
                ),
            }
        })
    }
}

async fn spawn_subject_mutation_test_room() -> (
    kameo::actor::ActorRef<RoomRegistryActor>,
    kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    jid::BareJid,
    Arc<waddle_xmpp::ownership::InProcessClaimStore>,
    waddle_xmpp::muc::RoomClaimFenceContext,
    Arc<SubjectMutationStore>,
) {
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, WireClusteringClaims};
    use waddle_xmpp::ownership::{InProcessClaimStore, NodeIdentity, SharedNodeIdentity};
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        OccupantIdSecret::new(b"subject-fail-closed-test-secret-32b".to_vec())
            .expect("test secret meets length floor"),
    ));
    let claim_store = Arc::new(InProcessClaimStore::new());
    let store = Arc::new(SubjectMutationStore::new(claim_store.clone()));
    room_registry
        .ask(WireClusteringClaims {
            claim_store: claim_store.clone(),
            node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                "subject-test-node",
                "subject-test-epoch",
            )),
            durable_store: Some(store.clone()),
            rollout_backoff: None,
        })
        .await
        .expect("wire subject mutation test store");
    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let room_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-subject".to_string(),
            channel_id: "c-subject".to_string(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create subject mutation test room");
    let snapshot = room_actor
        .ask(GetRoomSnapshot {
            sender_jid: "alice@example.com/web".parse().expect("sender full jid"),
        })
        .await
        .expect("subject mutation room snapshot");
    let claim_fence = snapshot
        .claim_fence
        .expect("durable subject mutation room has an exact fence");
    (
        room_registry,
        room_actor,
        room_jid,
        claim_store,
        claim_fence,
        store,
    )
}

fn subject_change_message(room: &jid::BareJid, sender: &jid::FullJid, text: &str) -> Message {
    let mut message = Message::new(Some(jid::Jid::from(room.clone())));
    message.from = Some(jid::Jid::from(sender.clone()));
    message.type_ = XmppMessageType::Groupchat;
    message
        .subjects
        .insert(xmpp_parsers::message::Lang::new(), text.to_string());
    message
}

fn persist_subject_event(
    room: &jid::BareJid,
    sender: &jid::FullJid,
    text: &str,
    claim_fence: waddle_xmpp::muc::RoomClaimFenceContext,
) -> OutboundEvent {
    use chrono::TimeZone;

    OutboundEvent::PersistRoomSubject {
        room: room.clone(),
        claim_fence: Some(claim_fence),
        texts: waddle_xmpp::muc::RoomSubjectTexts::from_iter([(String::new(), text.to_string())]),
        setter: sender.to_bare(),
        sender: sender.clone(),
        message: Box::new(subject_change_message(room, sender, text)),
        setter_nick: "alice-nick".to_string(),
        set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
    }
}

#[tokio::test]
async fn xep_0045_persist_room_subject_writes_state_via_room_actor() {
    // Per-arm coverage for `OutboundEvent::PersistRoomSubject`
    // (Copilot review, PR #319). Drives the event through
    // `interpret(...)` against a real `RoomRegistryActor` and a
    // pre-created room actor, then queries the room snapshot to
    // confirm the actor wrote `MucRoom.subject` to a `SubjectState`
    // matching the event payload.
    use chrono::TimeZone;
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::muc::room_registry_actor::CreateRoom;
    use waddle_xmpp::muc::RoomConfig;
    use waddle_xmpp::xep::xep0421::OccupantIdSecret;

    let registry = ConnectionRegistry::new();
    let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        "muc.example.com".to_string(),
        OccupantIdSecret::new(b"persist-subject-arm-test-secret-32b".to_vec())
            .expect("test secret meets length floor"),
    ));
    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let _room_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-1".to_string(),
            channel_id: "c-1".to_string(),
            config: RoomConfig::default(),
        })
        .await
        .expect("create room");

    let deps = Deps {
        connection_registry: &registry,
        user_registry: None,
        sm_session_registry: None,
        mam_storage: None,
        inbox_storage: None,
        extension_manager: None,
        room_registry: Some(&room_registry),
        web_socket_state: None,
        authenticated_session: None,
        local_domain: "example.com",
        blocking_storage: None,
        message_dispatcher: None,
        pending_delivery_storage: None,
        ordered_relay_origin: None,
    };

    let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
    ]);
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

    let events = vec![OutboundEvent::PersistRoomSubject {
        room: room_jid.clone(),
        claim_fence: None,
        texts: texts.clone(),
        setter: setter.clone(),
        sender: sender.clone(),
        message: Box::new(subject_change_message(
            &room_jid,
            &sender,
            "Default subject",
        )),
        setter_nick: "alice-nick".to_string(),
        set_at,
    }];
    let _outcome = interpret(events, &deps).await;

    // Verify the room actor wrote `SubjectState` matching the event payload.
    let actor = room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room actor present");
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot");
    let stored = snapshot
        .room
        .subject
        .expect("PersistRoomSubject must land a SubjectState");
    assert_eq!(stored.texts, texts);
    assert_eq!(stored.setter, setter);
    assert_eq!(stored.setter_nick, "alice-nick");
    assert_eq!(stored.set_at, set_at);
}

#[tokio::test]
async fn xep_0045_persist_room_subject_with_no_registry_bounces_and_halts_batch() {
    // A subject effect cannot safely complete without its room registry.
    // Reject it and suppress all later effects from the same dispatch batch.
    use chrono::TimeZone;

    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);

    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let texts =
        waddle_xmpp::muc::RoomSubjectTexts::from_iter([(String::new(), "ignored".to_string())]);
    let events = vec![
        OutboundEvent::PersistRoomSubject {
            room: room_jid.clone(),
            claim_fence: None,
            texts,
            setter,
            sender: sender.clone(),
            message: Box::new(subject_change_message(&room_jid, &sender, "ignored")),
            setter_nick: "alice-nick".to_string(),
            set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
        },
        OutboundEvent::CloseTransport,
    ];
    let outcome = interpret(events, &deps).await;
    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
}

#[tokio::test]
async fn xep_0045_stale_subject_effect_cannot_mutate_same_jid_successor() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DemoteRoomIfExactActor};
    use waddle_xmpp::ownership::{ClaimStore, ExactReleaseOutcome};

    let (room_registry, original_actor, room_jid, claim_store, original_fence, _store) =
        spawn_subject_mutation_test_room().await;

    assert_eq!(
        claim_store
            .release_exact(
                &original_fence.entity,
                &original_fence.owner,
                original_fence.epoch,
            )
            .await
            .expect("release original exact claim"),
        ExactReleaseOutcome::Released,
    );
    assert!(room_registry
        .ask(DemoteRoomIfExactActor {
            room_jid: room_jid.clone(),
            actor_ref: original_actor,
        })
        .await
        .expect("remove original actor"));
    let successor_actor = room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w-subject-successor".to_string(),
            channel_id: "c-subject-successor".to_string(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create same-JID successor");
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let successor_snapshot = successor_actor
        .ask(GetRoomSnapshot {
            sender_jid: sender.clone(),
        })
        .await
        .expect("successor chain snapshot");
    assert_ne!(
        successor_snapshot.claim_fence.as_ref(),
        Some(&original_fence),
        "the replacement must have a distinct exact authority"
    );

    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let outcome = interpret(
        vec![
            persist_subject_event(
                &room_jid,
                &sender,
                "stale predecessor subject",
                original_fence,
            ),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
    let current_actor = room_registry
        .ask(GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("lookup successor")
        .expect("successor remains registered");
    assert_eq!(current_actor.id(), successor_actor.id());
    assert!(
        successor_actor
            .ask(GetSnapshot)
            .await
            .expect("successor state snapshot")
            .room
            .subject
            .is_none(),
        "the predecessor's subject must never reach the successor"
    );
}

#[tokio::test]
async fn xep_0045_subject_not_owner_bounces_demotes_exact_actor_and_halts_batch() {
    let (room_registry, _room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::NotOwner);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "rejected subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(
        !outcome.close,
        "the event following rejected subject persistence must not be interpreted"
    );
    assert!(
        room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after demotion")
            .is_none(),
        "the exact actor that proved ownership loss must be demoted"
    );
}

#[tokio::test]
async fn xep_0045_subject_ownership_loss_during_persist_bounces_and_demotes() {
    let (room_registry, _room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::OwnershipLostDuringPersist);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");
    let mut subject_event =
        persist_subject_event(&room_jid, &sender, "stale in-memory subject", claim_fence);
    let OutboundEvent::PersistRoomSubject { message, .. } = &mut subject_event else {
        unreachable!("helper always builds a subject event")
    };
    message.payloads.push(waddle_xmpp::xep::build_hint_element(
        waddle_xmpp::xep::Hint::NoStore,
    ));

    let outcome = interpret(vec![subject_event, OutboundEvent::CloseTransport], &deps).await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "the post-subject batch must be suppressed");
    assert!(
        room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after write-adjacent loss")
            .is_none(),
        "the actor whose write-adjacent fence failed must be demoted"
    );
}

#[tokio::test]
async fn xep_0045_subject_ownership_unavailable_bounces_without_demotion_and_halts_batch() {
    let (room_registry, _room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::OwnershipUnavailable);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "ambiguous subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(
        !outcome.close,
        "the event following ambiguous subject persistence must not be interpreted"
    );
    assert!(
        room_registry
            .ask(GetRoom {
                room_jid: room_jid.clone(),
            })
            .await
            .expect("lookup after ambiguous mutation")
            .is_some(),
        "an ambiguous ownership probe must not demote the actor"
    );
}

#[tokio::test]
async fn xep_0045_subject_persist_failure_bounces_before_apply_and_halts_batch() {
    use waddle_xmpp::muc::room_actor::GetSnapshot;

    let (room_registry, room_actor, room_jid, _claim_store, claim_fence, store) =
        spawn_subject_mutation_test_room().await;
    store.set_mode(SubjectMutationStoreMode::PersistFailed);
    let connection_registry = ConnectionRegistry::new();
    let mut deps = Deps::registry_only(&connection_registry);
    deps.room_registry = Some(&room_registry);
    let sender: jid::FullJid = "alice@example.com/web".parse().expect("sender full jid");

    let outcome = interpret(
        vec![
            persist_subject_event(&room_jid, &sender, "rejected subject", claim_fence),
            OutboundEvent::CloseTransport,
        ],
        &deps,
    )
    .await;

    assert_eq!(outcome.frames.len(), 1, "sender receives one retry bounce");
    assert!(outcome.frames[0].contains("resource-constraint"));
    assert!(!outcome.close, "later effects must be suppressed");
    let snapshot = room_actor.ask(GetSnapshot).await.expect("room snapshot");
    assert!(
        snapshot.room.subject.is_none(),
        "failed durable persistence must leave in-memory subject unchanged"
    );
}

// -----------------------------------------------------------------
// XEP-0424 — groupchat retraction lookup (#281)
// -----------------------------------------------------------------

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
    let row = MamArchivedMessage {
        id: archive_pk.to_string(),
        timestamp: chrono::Utc::now(),
        from: format!("{room}/alice").parse().expect("room/nick jid"),
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
        stanza_xml: Some(format!(
            r#"<message xmlns='jabber:client' type='groupchat' from='{room}/alice' id='{wire_id}'><body>remove me</body><stanza-id xmlns='urn:xmpp:sid:0' by='{room}' id='{archive_pk}'/></message>"#
        )),
        rich: None,
        nickname_generation: Some(0),
    };
    mam.store_message(room, &row).await.expect("seed mam row");
    row
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_target_resolved_by_room_stanza_id() {
    // XEP-0424 §3 (xep-0424.xml lines 158, 230-232): a groupchat
    // `<retract id='...'/>` cites the room-assigned XEP-0359 stanza-id
    // (the `<stanza-id by='room'/>` value), NOT the original wire `id`
    // attribute. That stanza-id is persisted as the archive primary key
    // (see `finish_archive_groupchat_message`). waddle's own chat client
    // is conformant — it sends `replyableId = stampedByRoom` (the room
    // stanza-id). The retraction target lookup MUST resolve that id.
    //
    // Regression for the channel-delete failure: the lookup keyed off the
    // `stanza_id` column (which stores the wire `id` attribute), so a
    // conformant retraction citing the room stanza-id returned
    // `<item-not-found/>` and channel deletes silently did nothing.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room: jid::BareJid = "retract-by-stanza-id@muc.example.com"
        .parse()
        .expect("room");
    let room_stanza_id = "room-stamp-uuid-CCC";
    let wire_id = "alice-orig-3";
    seed_groupchat_archive_row(&mam, &room, room_stanza_id, wire_id).await;

    let resolved = lookup_groupchat_retraction_target(&mam, &room, room_stanza_id)
        .await
        .expect("lookup must not error")
        .expect("retraction citing the room stanza-id must resolve the seeded archive row");
    assert_eq!(
        resolved.id, room_stanza_id,
        "lookup_groupchat_retraction_target resolves the row by its room XEP-0359 stanza-id (archive PK)"
    );
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_target_rejects_wire_id_and_origin_id() {
    // Strict XEP-0424 §3 (xep-0424.xml lines 158, 230-232): a groupchat
    // retraction resolves ONLY by the room-assigned XEP-0359 stanza-id
    // (the archive primary key). A citation of the original wire `id`
    // attribute (the `stanza_id` column) or the client origin-id MUST
    // NOT resolve — the server replies `<item-not-found/>`, matching the
    // XEP-0425 moderation target semantics.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room: jid::BareJid = "retract-strict@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-AAA";
    let wire_id = "alice-orig-1";
    let origin_id = "alice-origin-1";
    // One row carrying three distinct ids: PK (room stanza-id), the wire
    // `id` attribute (`stanza_id` column), and a client origin-id.
    let row = MamArchivedMessage {
        id: archive_pk.to_string(),
        timestamp: chrono::Utc::now(),
        from: format!("{room}/alice").parse().expect("room/nick jid"),
        to: jid::Jid::from(room.clone()),
        body: Some("remove me".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            wire_id,
            jid::Jid::from(room.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new(origin_id)),
        message_type: XmppMessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: Some(0),
    };
    mam.store_message(&room, &row).await.expect("seed mam row");

    // Conformant id (room stanza-id = archive PK) resolves.
    assert!(
        lookup_groupchat_retraction_target(&mam, &room, archive_pk)
            .await
            .expect("lookup must not error")
            .is_some(),
        "retraction citing the room stanza-id must resolve"
    );
    // Wire `id` attribute (the `stanza_id` column) must NOT resolve.
    assert!(
        lookup_groupchat_retraction_target(&mam, &room, wire_id)
            .await
            .expect("lookup must not error")
            .is_none(),
        "retraction citing the wire id must not resolve — XEP-0424 §3 uses the room stanza-id"
    );
    // Client origin-id must NOT resolve.
    assert!(
        lookup_groupchat_retraction_target(&mam, &room, origin_id)
            .await
            .expect("lookup must not error")
            .is_none(),
        "retraction citing the client origin-id must not resolve"
    );
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_lookup_is_room_scoped() {
    // Resolution is scoped to the room: the room stanza-id (archive PK)
    // of a message in room A must not satisfy a retraction addressed to
    // room B. `lookup_groupchat_retraction_target` resolves by PK via
    // `get_message` and rejects any row whose archived `to` is a
    // different room.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room_a: jid::BareJid = "room-a@muc.example.com".parse().expect("room a");
    let room_b: jid::BareJid = "room-b@muc.example.com".parse().expect("room b");
    let archive_pk = "pk-A";
    seed_groupchat_archive_row(&mam, &room_a, archive_pk, "alice-orig-1").await;

    // Room A's room stanza-id queried under room B — must not resolve.
    let cross = lookup_groupchat_retraction_target(&mam, &room_b, archive_pk)
        .await
        .expect("lookup must not error");
    assert!(
        cross.is_none(),
        "groupchat retraction lookup must not return a row from a different room's archive"
    );
}

#[tokio::test]
async fn xep_0424_apply_groupchat_retraction_tombstone_keys_off_room_stanza_id() {
    // The tombstone-application site must resolve the target by the
    // room-assigned XEP-0359 stanza-id (the archive primary key) — the
    // id a conformant XEP-0424 client cites. Drives the
    // `OutboundEvent::ApplyGroupchatRetractionTombstone` arm of
    // `interpret` and asserts the seeded row gets its body
    // scrubbed and a `<retracted/>` tombstone written in its
    // place.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let room: jid::BareJid = "retract-tombstone@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-BBB";
    let wire_id = "alice-orig-2";
    seed_groupchat_archive_row(&mam, &room, archive_pk, wire_id).await;

    // Build the retraction message the chain would emit.
    let mut retraction = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    retraction.id = Some(xmpp_parsers::message::Id("retract-stanza-1".to_string()));
    retraction.from = Some(format!("{room}/alice").parse().expect("room/nick"));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(archive_pk));

    let events = vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
        room: room.clone(),
        target_message_id: archive_pk.to_string(),
        retraction_message: Box::new(retraction),
    }];
    let _outcome = interpret(events, &deps).await;

    // The seeded row's body must now be scrubbed and a
    // `<retracted/>` payload must replace it (XEP-0424 §"prevent
    // further distribution"). Read the row back by its room stanza-id
    // (archive primary key) — the same id the retraction cited.
    let row = mam
        .get_message(archive_pk)
        .await
        .expect("post-tombstone lookup")
        .expect("row still present after tombstone replace");
    assert!(row.body.is_none(), "tombstone must clear the original body");
    let rich = row
        .rich
        .as_ref()
        .expect("post-tombstone row must carry an ArchivedRichMessage");
    match rich.payload.as_ref() {
        Some(waddle_xmpp::mam::ArchivedRichPayload::Tombstone(ts)) => {
            assert_eq!(
                ts.retraction_id.as_ref().map(|id| id.as_str()),
                Some("retract-stanza-1"),
                "tombstone cites the retraction stanza id"
            );
        }
        other => {
            panic!("expected ArchivedRichPayload::Tombstone after retraction, got {other:?}")
        }
    }
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_scrubs_pending_delivery_rows() {
    // F2: promotion (#1097/#1098) parks undelivered copies in
    // pending_delivery. The retraction arm must scrub that layer with
    // the same keys as the SM-queue scrub, or the retracted content
    // (Transient rows) / a tombstone stub (Archived pointers) delivers
    // at the recipient's next login.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::pending_delivery::storage::{
        InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
    };
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.pending_delivery_storage = Some(&pending);

    let room: jid::BareJid = "retract-pending@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-PPP";
    let wire_id = "alice-orig-3";
    seed_groupchat_archive_row(&mam, &room, archive_pk, wire_id).await;

    // A promoted Archived pointer row for an offline member, keyed by
    // the room's XEP-0359 stamp (archive id == wire stanza-id
    // invariant), plus an unrelated row that must survive.
    let recipient: jid::BareJid = "offline@example.com".parse().expect("jid");
    pending
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                archive_pk,
                jid::Jid::from(room.clone()),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert");
    pending
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                "unrelated-stamp",
                jid::Jid::from(room.clone()),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert");

    let mut retraction = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    retraction.id = Some(xmpp_parsers::message::Id("retract-stanza-2".to_string()));
    retraction.from = Some(format!("{room}/alice").parse().expect("room/nick"));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(archive_pk));

    let events = vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
        room: room.clone(),
        target_message_id: archive_pk.to_string(),
        retraction_message: Box::new(retraction),
    }];
    let _outcome = interpret(events, &deps).await;

    let rows = pending.list(&recipient).await.expect("list");
    assert_eq!(
        rows.len(),
        1,
        "retraction must scrub the matching pending row; unrelated rows survive"
    );
    match &rows[0].payload {
        PendingPayload::Archived(r) => assert_eq!(r.id.as_str(), "unrelated-stamp"),
        other => panic!("expected surviving Archived row, got {other:?}"),
    }
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
        authenticated_session: None,
        local_domain: "example.com",
        blocking_storage: Some(&blocking),
        message_dispatcher: Some(&dispatcher),
        pending_delivery_storage: None,
        ordered_relay_origin: None,
    };

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "must arrive");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
        stanza: Box::new(Stanza::Message(msg)),
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
        let mut m = chat_msg("alice@example.com/web", "bob@example.com", "retry me");
        m.payloads.push(build_origin_id_element("origin-retry-1"));
        m
    };

    // First delivery: archives a row under bob's recipient stamp.
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(dm())),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com/gone", "hi bob");
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/gone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com/gone", "offline?");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/gone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
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
        inbound_count: 0,
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
        "alice@example.com/web",
        "bob@example.com/phone",
        "resume me",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/phone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
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

// ---------------------------------------------------------------------
// #1246 — RFC 6121 §8.5.1: message to a nonexistent local account is
// bounced with <service-unavailable/>, never persisted.
// ---------------------------------------------------------------------

#[tokio::test]
async fn route_bare_jid_message_to_nonexistent_local_user_bounces() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&state),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg("alice@example.com/web", "typo@example.com", "anyone?");
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "typo@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }],
        &deps,
    )
    .await;

    assert_eq!(
        outcome.frames.len(),
        1,
        "sender must receive a bounce for a nonexistent local account"
    );
    assert!(
        outcome.frames[0].contains("service-unavailable"),
        "RFC 6121 §8.5.1: the bounce is <service-unavailable/>; got {}",
        outcome.frames[0]
    );
    assert!(
        outcome.frames[0].contains("type=\"error\"") || outcome.frames[0].contains("type='error'"),
        "bounce is a message of type error; got {}",
        outcome.frames[0]
    );

    let typo_bare: jid::BareJid = "typo@example.com".parse().expect("bare");
    let typo_archive = mam
        .query_messages(
            &typo_bare,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query typo");
    assert!(
        typo_archive.messages.is_empty(),
        "no MAM rows may be created for a nonexistent account"
    );
}

#[tokio::test]
async fn route_bare_jid_message_to_existing_oidc_user_persists_offline() {
    // Two-table identity: an OIDC-provisioned account exists only in
    // `users` (no native_users row). The existence gate must accept it
    // and run the normal offline/headless persistence.
    use crate::db::actor::DbExecute;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: "INSERT INTO users \
                  (jid, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                "bob@example.com".into(),
                "bob".into(),
                "bob".into(),
                "Bob".into(),
                crate::db::Value::NullText,
                crate::db::Value::NullText,
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ],
        })
        .await
        .expect("seed oidc user");

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
    let blocking: Arc<dyn BlockingStorage> = Arc::new(InMemoryBlockingStorage::new());
    let dispatcher = pipelined_dispatcher();
    let deps = Deps {
        web_socket_state: Some(&state),
        ..offline_pass_deps(&registry, &mam, &inbox, &blocking, &dispatcher)
    };

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hello bob");
    let outcome = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
        }],
        &deps,
    )
    .await;
    assert!(
        outcome.frames.is_empty(),
        "existing OIDC account must not be bounced"
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
        "offline persistence runs for the OIDC-only account"
    );
}

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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "store me");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
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
        "alice@example.com/web",
        "bob@example.com/phone",
        "maybe blocked",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com/phone".parse::<jid::Jid>().expect("full"),
            stanza: Box::new(Stanza::Message(msg)),
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

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "bare detached");
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
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
        "alice@example.com/web",
        "bob@example.com",
        "should not pass",
    );
    let _ = interpret(
        vec![OutboundEvent::RouteToConnection {
            jid: "bob@example.com".parse::<jid::Jid>().expect("bare"),
            stanza: Box::new(Stanza::Message(msg)),
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
