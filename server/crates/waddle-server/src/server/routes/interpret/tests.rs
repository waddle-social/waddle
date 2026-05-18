use super::bot::{dispatch_bot_groupchat_response, BotGroupchatDispatch};
use super::groupchat_archive::room_scoped_reply_to_attr;
use super::groupchat_validation::lookup_groupchat_retraction_target;
use super::*;
use waddle_xmpp::xep::{set_thread_create, ThreadCreate};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::minidom::Element;

fn test_registry() -> ConnectionRegistry {
    ConnectionRegistry::new()
}

fn result_iq(id: &str) -> Iq {
    Iq {
        from: None,
        to: None,
        id: id.to_string(),
        payload: IqType::Result(Some(Element::builder("query", "jabber:iq:roster").build())),
    }
}

#[tokio::test]
async fn interprets_send_stanza() {
    let events = vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq(
        "x",
    ))))];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
    assert_eq!(outcome.frames.len(), 1);
    assert!(outcome.frames[0].contains("type=\"result\""));
    assert!(outcome.frames[0].contains("id=\"x\""));
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
        .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
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
        exclude: alice_web,
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
        exclude: alice_web,
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
        exclude: alice_web,
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
        exclude: bob_desk,
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
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
    };
    sm.store_session(detached).await.expect("store session");

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let original = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let deps = Deps {
        connection_registry: &registry,
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
    };
    let _outcome = interpret(
        vec![OutboundEvent::SendCarbons {
            owner: owner.clone(),
            message: Box::new(original),
            kind: CarbonKind::Sent,
            exclude: alice_web,
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
    msg.id = Some("orig-1".to_string());

    let events = vec![OutboundEvent::ArchiveDirect {
        archive_jid: archive_jid.clone(),
        from,
        to,
        message: Box::new(msg),
    }];
    let _outcome = interpret(events, &deps).await;

    let stored = mam
        .query_messages(&archive_jid, &Default::default())
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
    msg.id = Some("wire-id".to_string());
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
            from: alice.clone(),
            to: bob.clone(),
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
            from: alice.clone(),
            to: bob.clone(),
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
            from: alice.clone(),
            to: bob.clone(),
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
            from: alice.clone(),
            to: bob.clone(),
            message: Box::new(msg.clone()),
        },
        OutboundEvent::ArchiveDirect {
            archive_jid: bob.clone(),
            from: alice.clone(),
            to: bob.clone(),
            message: Box::new(msg),
        },
    ];
    let _outcome = interpret(events, &deps).await;

    let alice_archive = mam
        .query_messages(&alice, &Default::default())
        .await
        .expect("query alice");
    let bob_archive = mam
        .query_messages(&bob, &Default::default())
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
        from: alice,
        to: bob,
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
    msg.id = Some("origin-X".to_string());

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
                archived.message.bodies.get("").map(|b| b.0.clone()),
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
        reference: MessageRef::OriginId {
            sender: alice_bare.clone(),
            origin_id: OriginId::new("collision"),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(21),
            result: Some(archived),
        } => {
            let body = archived
                .message
                .bodies
                .get("")
                .map(|b| b.0.clone())
                .unwrap_or_default();
            assert_eq!(
                body, "from alice",
                "OriginId lookup must scope to sender; bob's row was a collision decoy"
            );
        }
        other => panic!("expected alice-authored row, got {other:?}"),
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
        reference: MessageRef::OriginId {
            sender: charlie_bare,
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
        }),
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(13),
        archive: archive_jid.clone(),
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
    original.id = Some("orig-id".to_string());

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
                message.bodies.get("").map(|b| b.0.clone()),
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
    original.id = Some("fail-open-id".to_string());
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
                message.id.as_deref(),
                Some("fail-open-id"),
                "fail-open path returns the original message id"
            );
            assert_eq!(
                message.bodies.get("").map(|b| b.0.clone()),
                original.bodies.get("").map(|b| b.0.clone()),
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
    original.id = Some("e-id".to_string());

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
            assert_eq!(message.id.as_deref(), Some("e-id"));
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
    use waddle_xmpp::registry::DeliveryKind;
    let registry = ConnectionRegistry::new();
    let bob: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(bob.clone(), bob_tx, false);

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: jid::Jid::from(bob.clone()),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

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

#[tokio::test]
async fn route_to_connection_bare_jid_selects_highest_priority_available_resources() {
    // RFC 6121 §8.5.2.1 resource selection: deliver to every
    // resource tied at the highest available priority. A
    // bare-JID `to` from the sender pass (handlers/route.rs
    // emits `message.to` verbatim) lands here; without selection
    // the cutover would silently drop bare-targeted 1:1 traffic.
    use waddle_xmpp::registry::DeliveryKind;
    let registry = ConnectionRegistry::new();
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let bob_phone: jid::FullJid = "bob@example.com/phone".parse().expect("jid");
    let bob_tablet: jid::FullJid = "bob@example.com/tablet".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    let (phone_tx, mut phone_rx) = tokio::sync::mpsc::channel(8);
    let (tablet_tx, mut tablet_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
    registry.register_with_carbons(bob_phone.clone(), phone_tx, false);
    registry.register_with_carbons(bob_tablet.clone(), tablet_tx, false);
    // desk + phone available at priority 5 (tied); tablet at
    // lower priority 1. Tablet must NOT receive.
    registry.update_presence(&bob_desk, true, 5);
    registry.update_presence(&bob_phone, true, 5);
    registry.update_presence(&bob_tablet, true, 1);

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bare");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

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
    let registry = ConnectionRegistry::new();
    let bob_desk: jid::FullJid = "bob@example.com/desk".parse().expect("jid");
    let (desk_tx, mut desk_rx) = tokio::sync::mpsc::channel(8);
    registry.register_with_carbons(bob_desk.clone(), desk_tx, false);
    // Registered but presence NOT made available — legacy
    // routing still delivers to this resource.

    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi");
    let events = vec![OutboundEvent::RouteToConnection {
        jid: "bob@example.com".parse::<jid::Jid>().expect("bare jid"),
        stanza: Box::new(Stanza::Message(msg)),
    }];
    let _outcome = interpret(events, &Deps::registry_only(&registry)).await;

    let delivered = drain_inbound(&mut desk_rx);
    assert_eq!(
        delivered.len(),
        1,
        "no presence -> still delivered to connected resource as a legacy fallback"
    );
}

#[tokio::test]
async fn preserves_frame_order_across_multiple_events() {
    let events = vec![
        OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("a")))),
        OutboundEvent::Log {
            level: tracing::Level::DEBUG,
            message: "between".to_string(),
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Iq(result_iq("b")))),
    ];
    let outcome = interpret(events, &Deps::registry_only(&test_registry())).await;
    assert_eq!(outcome.frames.len(), 2);
    assert!(outcome.frames[0].contains("id=\"a\""));
    assert!(outcome.frames[1].contains("id=\"b\""));
}

#[tokio::test]
async fn send_stanza_preserves_xep_0201_thread_on_wire() {
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "threaded hi");
    msg.thread = Some(xmpp_parsers::message::Thread("root-thread".to_string()));

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
    use waddle_extensions::{DisplayText, FullJidValue, ReplyTarget, RoomJid, StanzaId, ThreadId};

    let registry = ConnectionRegistry::new();
    let room_jid: jid::BareJid = "chat@muc.example.com".parse().expect("room jid");
    let alice: jid::FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: jid::FullJid = "bob@example.com/web".parse().expect("bob jid");
    let bot: jid::FullJid = "chat@example.com/bot".parse().expect("bot jid");
    let (alice_tx, mut alice_rx) = tokio::sync::mpsc::channel(8);
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel(8);
    registry.register(alice.clone(), alice_tx);
    registry.register(bob.clone(), bob_tx);

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
        extensions: None,
    };

    let test_secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
        b"test-occupant-id-secret-32-bytes-long".to_vec(),
    )
    .expect("test secret meets length floor");
    let outcome = dispatch_bot_groupchat_response(
        &Deps::registry_only(&registry),
        BotGroupchatDispatch {
            room_jid: &room_jid,
            occupants: &occupants,
            durable_recipient_bare_jids: &[],
            sender_full: &bot,
            room_actor: None,
            room_moderated: false,
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
        message.thread.as_ref().map(|thread| thread.0.as_str()),
        Some("root-msg")
    );
    assert_eq!(
        message.bodies.get("").map(|body| body.0.as_str()),
        Some("bot answer")
    );
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
    message.id = Some("live-forum-root".to_string());
    message.type_ = xmpp_parsers::message::MessageType::Groupchat;
    set_thread_create(&mut message, &ThreadCreate::new("Live forum root"));

    let thread_id = normalize_thread_create_source(&mut message);

    assert_eq!(thread_id.as_deref(), Some("live-forum-root"));
    assert_eq!(
        message.thread.as_ref().map(|thread| thread.0.as_str()),
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
        .query_messages(&bob_bare, &Default::default())
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
        .query_messages(&bob, &Default::default())
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
        .query_messages(&bob_bare, &Default::default())
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
        .query_messages(&bob, &Default::default())
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
    wire_msg.id = Some("wire-id".to_string());
    wire_msg.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("wire-trace body".to_string()),
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
        .query_messages(&alice_bare, &Default::default())
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
            .map(|xml| xml.contains("by=\"alice@example.com\""))
            .unwrap_or(false),
        "alice archive entry carries XEP-0359 <stanza-id by='alice@example.com'/>: \
         {:?}",
        alice_archive.messages[0].stanza_xml
    );

    // bob's MAM has 1 entry; <stanza-id by='bob@example.com'>
    // present (recipient-side stamp by the headless pass).
    let bob_archive = mam
        .query_messages(&bob, &Default::default())
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
            .map(|xml| xml.contains("by=\"bob@example.com\""))
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
            !frame.contains("to=\"bob@example.com\""),
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
        .query_messages(&bob_remote, &Default::default())
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
    let room_registry = kameo::spawn(RoomRegistryActor::new(
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
    };

    let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
    ]);
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

    let events = vec![OutboundEvent::PersistRoomSubject {
        room: room_jid.clone(),
        texts: texts.clone(),
        setter: setter.clone(),
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
async fn xep_0045_persist_room_subject_with_no_registry_is_noop() {
    // Defensive coverage for the `room_registry: None` skip arm —
    // a `PersistRoomSubject` arriving in a deployment without a
    // room registry must be logged-and-skipped, not panicked.
    use chrono::TimeZone;

    let registry = ConnectionRegistry::new();
    let deps = Deps::registry_only(&registry);

    let room_jid: jid::BareJid = "channel@muc.example.com".parse().expect("bare jid");
    let setter: jid::BareJid = "alice@example.com".parse().expect("setter bare jid");
    let texts =
        waddle_xmpp::muc::RoomSubjectTexts::from_iter([(String::new(), "ignored".to_string())]);
    let events = vec![OutboundEvent::PersistRoomSubject {
        room: room_jid,
        texts,
        setter,
        setter_nick: "alice-nick".to_string(),
        set_at: chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap(),
    }];
    let outcome = interpret(events, &deps).await;
    assert!(outcome.frames.is_empty());
    assert!(!outcome.close);
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
async fn xep_0424_groupchat_retraction_target_looked_up_by_message_id_not_archive_pk() {
    // Regression for #281: the groupchat retraction lookup must
    // match the wire `<retract id='...'/>` against the archive
    // row's stored wire message id (i.e. `stanza_id` column),
    // never against the storage primary key. The bug returned
    // `<item-not-found/>` to legitimate retractions because the
    // PK-based lookup only happened to match when the storage
    // backend coincidentally collided `id` with `stanza_id`.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room: jid::BareJid = "retract-l1@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-AAA";
    let wire_id = "alice-orig-1";
    seed_groupchat_archive_row(&mam, &room, archive_pk, wire_id).await;

    // Lookup by the wire id resolves the row.
    let resolved = lookup_groupchat_retraction_target(&mam, &room, wire_id)
        .await
        .expect("lookup must not error");
    let resolved =
        resolved.expect("wire-id retraction target must resolve to the seeded archive row");
    assert_eq!(
        resolved.id, archive_pk,
        "lookup_groupchat_retraction_target returned the seeded row keyed by wire id"
    );
    assert_eq!(
        resolved
            .stanza_id
            .as_ref()
            .map(|s| s.id.as_str())
            .unwrap_or_default(),
        wire_id,
        "resolved row's wire stanza_id matches the retraction target"
    );

    // Lookup by the archive PK must NOT match — that was the
    // pre-fix behavior the issue called out.
    let pk_lookup = lookup_groupchat_retraction_target(&mam, &room, archive_pk)
        .await
        .expect("lookup must not error");
    assert!(
        pk_lookup.is_none(),
        "PK lookup must not satisfy a retraction whose target id is a wire id"
    );
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_lookup_is_room_scoped() {
    // The accessor must scope the lookup to the room archive so
    // a colliding wire id in another room does not satisfy the
    // retraction. (The pre-fix global `get_message` PK lookup
    // performed the room scope via a manual `to.to_bare()`
    // post-filter; the new accessor scopes via the SQL/in-memory
    // archive_jid predicate. Either path must reject cross-room
    // matches.)
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room_a: jid::BareJid = "room-a@muc.example.com".parse().expect("room a");
    let room_b: jid::BareJid = "room-b@muc.example.com".parse().expect("room b");
    let wire_id = "shared-wire-id-1";
    seed_groupchat_archive_row(&mam, &room_a, "pk-A", wire_id).await;

    // Same wire id queried under room B — must not resolve.
    let cross = lookup_groupchat_retraction_target(&mam, &room_b, wire_id)
        .await
        .expect("lookup must not error");
    assert!(
        cross.is_none(),
        "groupchat retraction lookup must not return rows from a different room archive"
    );
}

#[tokio::test]
async fn xep_0424_apply_groupchat_retraction_tombstone_keys_off_wire_id() {
    // Regression for #281's second defective site: the tombstone
    // application must also key off the wire id. Drives the
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
    retraction.id = Some("retract-stanza-1".to_string());
    retraction.from = Some(format!("{room}/alice").parse().expect("room/nick"));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(wire_id));

    let events = vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
        room: room.clone(),
        target_message_id: wire_id.to_string(),
        retraction_message: Box::new(retraction),
    }];
    let _outcome = interpret(events, &deps).await;

    // The seeded row's body must now be scrubbed and a
    // `<retracted/>` payload must replace it (XEP-0424 §"prevent
    // further distribution"). Reading the row back via the same
    // wire-id accessor proves both sites agree on the lookup key.
    let row = mam
        .get_message_by_message_id(&room, wire_id)
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
