use super::*;

async fn current_admission_revision(
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
) -> u64 {
    room_actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .admission_revision
}

fn dm_call_message(from: &str, to: &str, payload: Element) -> xmpp_parsers::message::Message {
    let mut message = xmpp_parsers::message::Message::new(Some(to.parse().expect("to jid")));
    message.from = Some(from.parse().expect("from jid"));
    message.type_ = XmppMessageType::Chat;
    message.payloads.push(payload);
    message.payloads.push(waddle_xmpp::xep::build_hint_element(
        waddle_xmpp::xep::Hint::Store,
    ));
    message
}

#[tokio::test]
async fn muc_pm_sender_archive_uses_typed_tuple_for_ancillary_projections() {
    use crate::notification_activity::NotificationActivityReader;

    let state = create_test_websocket_state().await;
    let sender: BareJid = "alice@example.com".parse().expect("sender bare jid");
    let room: BareJid = "room@muc.example.com".parse().expect("room bare jid");
    let target: jid::FullJid = "room@muc.example.com/bob"
        .parse()
        .expect("target occupant jid");
    let sid = xmpp_parsers::jingle::SessionId("muc-pm-call-projection".to_owned());
    let propose = waddle_xmpp::xep::xep0353::build_propose(
        sid.clone(),
        waddle_xmpp::xep::xep0353::CallOffer::audio_video(),
    );
    // The archived row's typed tuple identifies the sender-owned pass. Its
    // serialized stanza may already carry the room-authored occupant `from`.
    let message = dm_call_message(
        "room@muc.example.com/alice",
        "room@muc.example.com/bob",
        propose,
    );
    let deps = build_interpret_deps(state.as_ref(), None);

    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
            archive_jid: sender.clone(),
            from: jid::Jid::from(sender.clone()),
            to: jid::Jid::from(target),
            message: Box::new(message),
        }],
        &deps,
    )
    .await;

    let expected_key = crate::server::routes::websocket::DmCallThreadKey::new(
        sender.clone(),
        room.clone(),
        sid.clone(),
    );
    assert!(
        state
            .deps
            .protocol
            .pending_dm_call_offers
            .contains_key(&expected_key),
        "sender-side MUC-PM call projection must remain sender -> room bare"
    );
    let wrong_self_key =
        crate::server::routes::websocket::DmCallThreadKey::new(sender.clone(), sender.clone(), sid);
    assert!(
        !state
            .deps
            .protocol
            .pending_dm_call_offers
            .contains_key(&wrong_self_key),
        "occupant wire `from` must not misclassify the sender archive as recipient-owned"
    );
    assert!(
        state
            .deps
            .protocol
            .notification_activity
            .read_activity(&sender, &room)
            .await
            .expect("read sender-room activity")
            .is_some(),
        "outbound activity must remain keyed by sender and room"
    );
    assert!(
        state
            .deps
            .protocol
            .notification_activity
            .read_activity(&sender, &sender)
            .await
            .expect("read wrong self activity")
            .is_none(),
        "sender-side MUC-PM must not create self-conversation activity"
    );
}

async fn drive_dm_message(
    state: &WebSocketState,
    full_jid: FullJid,
    message: xmpp_parsers::message::Message,
) {
    let phase = waddle_xmpp::protocol::ConnectionPhase::ready(full_jid.clone(), false);
    let mut sm = XmppStateMachine::new(
        "example.com".to_owned(),
        (*state.deps.protocol.dispatcher).clone(),
    );
    sm.transition_to_ready(full_jid, false);
    sm.set_blocklist(waddle_xmpp::protocol::session_state::Blocklist::empty());
    let _ =
        handlers::message::handle_message(message, state, &phase, Some(&mut sm), None, None, None)
            .await;
}

async fn store_committed_dm_archive_for_notification(
    state: &WebSocketState,
    archive_jid: &BareJid,
    archive_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    message: &xmpp_parsers::message::Message,
) {
    let from = message.from.clone().expect("archived message from");
    let to = message
        .to
        .clone()
        .unwrap_or_else(|| jid::Jid::from(archive_jid.clone()));
    let body = message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .cloned();
    let archived = waddle_xmpp::mam::ArchivedMessage {
        id: archive_stanza_id.id.clone(),
        body,
        stanza_id: Some(archive_stanza_id.clone()),
        message_type: message.type_.clone(),
        stanza_xml: Some(
            waddle_xmpp::parser::message_to_string(message).expect("serialize archived message"),
        ),
        ..waddle_xmpp::mam::ArchivedMessage::for_test(from, to)
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(archive_jid, &archived)
        .await
        .expect("store committed MAM row");
}

async fn archived_dm_call_anchor(
    state: &WebSocketState,
    owner: &BareJid,
    thread_id: &str,
) -> waddle_xmpp::mam::ArchivedMessage {
    state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            owner,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query DM MAM")
        .messages
        .into_iter()
        .find(|row| {
            row.thread
                .as_ref()
                .is_some_and(|thread| thread.id.as_str() == thread_id)
        })
        .unwrap_or_else(|| panic!("{owner} should have a MAM call-thread anchor"))
}

#[tokio::test]
async fn dm_jmi_proceed_projects_call_thread_anchor_for_both_peers() {
    let state = create_test_websocket_state().await;
    let alice_full: FullJid = "alice@example.com/web".parse().expect("alice full jid");
    let alice: BareJid = "alice@example.com".parse().expect("alice bare jid");
    let bob: BareJid = "bob@example.com".parse().expect("bob bare jid");
    let charlie: BareJid = "charlie@example.com".parse().expect("charlie bare jid");
    let sid = xmpp_parsers::jingle::SessionId("dm-call-1".to_owned());

    let propose = waddle_xmpp::xep::xep0353::build_propose(
        sid.clone(),
        waddle_xmpp::xep::xep0353::CallOffer::audio_video(),
    );
    drive_dm_message(
        state.as_ref(),
        alice_full,
        dm_call_message("alice@example.com/web", "bob@example.com/phone", propose),
    )
    .await;
    assert_eq!(state.deps.protocol.pending_dm_call_offers.len(), 1);
    let key = crate::server::routes::websocket::DmCallThreadKey::new(
        alice.clone(),
        bob.clone(),
        sid.clone(),
    );
    if let Some(mut offer) = state.deps.protocol.pending_dm_call_offers.get_mut(&key) {
        offer.started = chrono::Utc::now() - chrono::Duration::minutes(10);
    }

    let proceed: Element = waddle_xmpp::xep::xep0353::build_proceed(sid.clone()).into();
    drive_dm_message(
        state.as_ref(),
        "alice@example.com/web".parse().expect("alice full jid"),
        dm_call_message(
            "alice@example.com/web",
            "bob@example.com/phone",
            proceed.clone(),
        ),
    )
    .await;
    assert_eq!(
        state.deps.protocol.dm_call_threads.len(),
        0,
        "the proposer must not accept its own JMI offer"
    );

    let accepted_proceed =
        dm_call_message("bob@example.com/phone", "alice@example.com/web", proceed);
    let deps = build_interpret_deps(state.as_ref(), None);
    let _ = crate::server::routes::interpret::interpret(
        vec![
            waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
                archive_jid: bob.clone(),
                from: bob.clone().into(),
                to: alice.clone().into(),
                message: Box::new(accepted_proceed.clone()),
            },
            waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
                archive_jid: alice.clone(),
                from: bob.clone().into(),
                to: alice.clone().into(),
                message: Box::new(accepted_proceed),
            },
        ],
        &deps,
    )
    .await;

    let mut projected_stanza_ids = Vec::new();
    for (owner, peer) in [(&alice, &bob), (&bob, &alice)] {
        let archived_anchor = archived_dm_call_anchor(state.as_ref(), owner, "dm-call-1").await;
        let call_thread = archived_anchor
            .stanza_xml
            .as_deref()
            .and_then(|xml| xml.parse::<Element>().ok())
            .and_then(|message| {
                message
                    .children()
                    .find(|child| {
                        child.name() == "call-thread"
                            && child.ns() == waddle_xmpp::xep::NS_WADDLE_CALL_THREAD
                    })
                    .and_then(|child| waddle_xmpp::xep::parse_call_thread_anchor(child).ok())
            })
            .unwrap_or_else(|| panic!("{owner} MAM anchor should carry a call-thread marker"));
        assert_eq!(call_thread.kind, waddle_xmpp::xep::CallThreadKind::Dm);
        assert_eq!(call_thread.sid.0, "dm-call-1");
        assert_eq!(
            call_thread.media,
            waddle_xmpp::xep::CallThreadMedia::audio_video()
        );
        assert_eq!(call_thread.initiator, alice);
        assert!(
            chrono::Utc::now().signed_duration_since(call_thread.started)
                < chrono::Duration::seconds(5),
            "call duration anchor should start at accepted proceed time"
        );

        let anchor = state
            .deps
            .protocol
            .inbox_storage
            .list_threads(owner, peer)
            .await
            .expect("list DM threads")
            .into_iter()
            .find(|entry| entry.thread_id.as_deref() == Some("dm-call-1"))
            .unwrap_or_else(|| panic!("{owner} should have a DM call-thread anchor"));
        assert_eq!(
            anchor.call_thread_kind,
            Some(waddle_xmpp::xep::CallThreadKind::Dm)
        );
        assert_eq!(
            anchor.call_thread_media,
            Some(waddle_xmpp::xep::CallThreadMedia::audio_video())
        );
        projected_stanza_ids.push(anchor.last_stanza_id);
    }
    assert_ne!(
        projected_stanza_ids[0], projected_stanza_ids[1],
        "each owner should point at their own archived proceed row"
    );
    assert_eq!(state.deps.protocol.dm_call_threads.len(), 1);
    assert_eq!(state.deps.protocol.pending_dm_call_offers.len(), 0);
    assert_eq!(state.deps.protocol.dm_call_thread_projections.len(), 2);

    let alice_archive_before_self_proceed = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &alice,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query alice MAM before self proceed")
        .messages
        .len();
    state
        .deps
        .protocol
        .dm_call_thread_projections
        .remove(&(alice.clone(), key.clone()));
    let self_proceed: Element = waddle_xmpp::xep::xep0353::build_proceed(sid.clone()).into();
    let _ = crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(dm_call_message(
                "alice@example.com/web",
                "bob@example.com/phone",
                self_proceed,
            )),
        }],
        &deps,
    )
    .await;
    let alice_archive_after_self_proceed = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &alice,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query alice MAM after self proceed")
        .messages;
    assert_eq!(
        alice_archive_after_self_proceed.len(),
        alice_archive_before_self_proceed + 1,
        "self-authored proceed is still archived as an ordinary JMI stanza"
    );
    assert_eq!(
        alice_archive_after_self_proceed
            .iter()
            .filter(|row| row
                .thread
                .as_ref()
                .is_some_and(|thread| thread.id.as_str() == "dm-call-1"))
            .count(),
        1,
        "self-authored proceed must not be promoted into a second MAM anchor"
    );
    assert!(
        !state
            .deps
            .protocol
            .dm_call_thread_projections
            .contains(&(alice.clone(), key.clone())),
        "self-authored proceed must not backfill a missing owner projection"
    );
    state
        .deps
        .protocol
        .dm_call_thread_projections
        .insert((alice.clone(), key.clone()));

    let replayed_proceed: Element = waddle_xmpp::xep::xep0353::build_proceed(sid.clone()).into();
    let replayed_proceed = dm_call_message(
        "bob@example.com/phone",
        "alice@example.com/web",
        replayed_proceed,
    );
    let _ = crate::server::routes::interpret::interpret(
        vec![
            waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
                archive_jid: bob.clone(),
                from: bob.clone().into(),
                to: alice.clone().into(),
                message: Box::new(replayed_proceed.clone()),
            },
            waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
                archive_jid: alice.clone(),
                from: bob.clone().into(),
                to: alice.clone().into(),
                message: Box::new(replayed_proceed),
            },
        ],
        &deps,
    )
    .await;
    for ((owner, peer), expected_stanza_id) in [(&alice, &bob), (&bob, &alice)]
        .into_iter()
        .zip(projected_stanza_ids)
    {
        let anchor = state
            .deps
            .protocol
            .inbox_storage
            .list_threads(owner, peer)
            .await
            .expect("list DM threads after replay")
            .into_iter()
            .find(|entry| entry.thread_id.as_deref() == Some("dm-call-1"))
            .unwrap_or_else(|| panic!("{owner} should keep a DM call-thread anchor"));
        assert_eq!(
            anchor.last_stanza_id, expected_stanza_id,
            "duplicate proceed should not rewrite an existing owner projection"
        );
    }

    state
        .deps
        .protocol
        .inbox_storage
        .upsert(
            &alice,
            waddle_xmpp::inbox::InboxEntry::new(
                charlie.clone(),
                waddle_xmpp::inbox::ConversationKind::Direct,
                "charlie-anchor",
                crate::time::now_ms(),
            )
            .with_thread("dm-call-1")
            .with_call_thread(
                waddle_xmpp::xep::CallThreadKind::Dm,
                waddle_xmpp::xep::CallThreadMedia::audio_only(),
            ),
            false,
        )
        .await
        .expect("seed same-sid DM anchor for a different peer");

    let finish = waddle_xmpp::xep::xep0353::build_finish(sid, None);
    drive_dm_message(
        state.as_ref(),
        "alice@example.com/web".parse().expect("alice full jid"),
        dm_call_message("alice@example.com/web", "bob@example.com/phone", finish),
    )
    .await;
    assert_eq!(state.deps.protocol.dm_call_threads.len(), 0);
    assert_eq!(state.deps.protocol.dm_call_thread_projections.len(), 0);

    for (owner, peer) in [(&alice, &bob), (&bob, &alice)] {
        let anchor = state
            .deps
            .protocol
            .inbox_storage
            .list_threads(owner, peer)
            .await
            .expect("list ended DM threads")
            .into_iter()
            .find(|entry| entry.thread_id.as_deref() == Some("dm-call-1"))
            .unwrap_or_else(|| panic!("{owner} should keep the DM call-thread anchor"));
        assert!(
            anchor.call_ended_at.is_some(),
            "{owner} should see the DM call-thread ended timestamp"
        );
        assert!(
            anchor
                .call_duration
                .as_ref()
                .is_some_and(|duration| duration.as_str().starts_with("PT")),
            "{owner} should see an ISO-8601 DM call-thread duration"
        );
    }

    let charlie_anchor = state
        .deps
        .protocol
        .inbox_storage
        .list_threads(&alice, &charlie)
        .await
        .expect("list Charlie DM threads")
        .into_iter()
        .find(|entry| entry.thread_id.as_deref() == Some("dm-call-1"))
        .expect("Alice/Charlie same-sid anchor remains");
    assert!(
        charlie_anchor.call_ended_at.is_none(),
        "DM finish should only update the two exact owner/peer projections"
    );
}

async fn project_direct_unread_for_notification(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
    stanza_id: &str,
) {
    state
        .deps
        .protocol
        .inbox_storage
        .upsert(
            recipient,
            waddle_xmpp::inbox::InboxEntry::new(
                sender.clone(),
                waddle_xmpp::inbox::ConversationKind::Direct,
                stanza_id,
                crate::time::now_ms(),
            ),
            true,
        )
        .await
        .expect("project unread inbox entry");
}

async fn drain_notification_candidates_for_test(state: &WebSocketState) -> usize {
    let push_service_jid: BareJid = state
        .deps
        .service_domains
        .push
        .parse()
        .expect("push service jid");
    let room_policy =
        crate::room_policy::RoomRegistryActorPolicy::new(state.deps.protocol.room_registry.clone());
    let dnd_reader = crate::notification_outbox::NoopDndReader;
    let activity_reader = state.deps.protocol.notification_activity.as_ref();
    let deps = crate::notification_outbox::NotificationDrainDeps::new(
        &room_policy,
        &dnd_reader,
        activity_reader,
    );
    state
        .deps
        .protocol
        .notification_outbox
        .drain_pending_candidates_into_outbox(
            state.deps.protocol.push_store.as_ref(),
            state.deps.protocol.blocking_storage.as_ref(),
            state
                .deps
                .protocol
                .notification_settings_projection
                .as_ref(),
            deps,
            &push_service_jid,
            16,
        )
        .await
        .expect("drain notification candidates")
}

/// Seed a recent `notification_activity` row for a recipient against
/// a conversation so the slice 2b XEP-0513 `<active/>` filter passes
/// at T1. Mirrors the shape of a fresh XEP-0085 chat-state ingest:
/// `last_active_at_ms = now`, no chat-state / read-marker / presence
/// columns. Tests that exercise the XEP-0513 *miss* path call the
/// drain WITHOUT calling this helper.
async fn seed_notification_activity_for_test(
    state: &WebSocketState,
    owner: &BareJid,
    conversation: &BareJid,
) {
    state
        .deps
        .protocol
        .notification_activity
        .record_outbound_message(owner, conversation, crate::time::now_ms())
        .await
        .expect("seed notification_activity row");
}

async fn register_first_party_push_for_test(
    state: &WebSocketState,
    recipient: &BareJid,
    device_id: &str,
) {
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            recipient,
            crate::push_service::PushDeviceRegistration::new(
                device_id,
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");
}

#[tokio::test]
async fn queue_offline_delivery_publishes_first_party_xep0357_notification_without_touching_external_registrations(
) {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");
    state
        .deps
        .protocol
        .push_store
        .register(waddle_xmpp::push::PushSubscription {
            user_jid: recipient.to_string(),
            service_jid: "push-provider.example.com".to_string(),
            node: Some("external-web-node".to_string()),
            publish_options: Some(
                Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
                    .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                    .append(
                        Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                            .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                            .append(
                                Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                                    .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                            .attr(minidom::rxml::xml_ncname!("var").to_owned(), "secret")
                            .append(
                                Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                                    .append("external-provider-secret")
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            ),
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("external push registration");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id("offline-push-1".to_string()));
    message.type_ = XmppMessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "push me".to_string());
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-offline-push-1",
        jid::Jid::from(recipient.clone()),
    );
    store_committed_dm_archive_for_notification(&state, &recipient, &archive_stanza_id, &message)
        .await;
    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;
    let sender_bare: BareJid = "alice@example.com".parse().expect("sender bare jid");
    project_direct_unread_for_notification(
        state.as_ref(),
        &recipient,
        &sender_bare,
        "archive-offline-push-1",
    )
    .await;

    let attempts_before_drain = state
        .deps
        .protocol
        .push_service
        .delivery_attempts_for_node(node.node())
        .await
        .expect("delivery attempts before drain");
    assert!(attempts_before_drain.is_empty());
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(outbox_jobs.len(), 1);
    assert_eq!(outbox_jobs[0].message_count(), 1);
    assert_eq!(outbox_jobs[0].conversation_jid(), &sender_bare);
    let push_service_jid: BareJid = "push.example.com".parse().expect("push service jid");
    let outbox_results = state
        .deps
        .protocol
        .notification_outbox
        .drain_due_outbox_jobs(
            state.deps.protocol.push_service.as_ref(),
            state.deps.protocol.push_store.as_ref(),
            state.deps.protocol.inbox_storage.as_ref(),
            state.deps.protocol.blocking_storage.as_ref(),
            &push_service_jid,
            16,
        )
        .await
        .expect("drain notification outbox");
    assert_eq!(outbox_results.len(), 1);
    let queued_publish_jobs = state
        .deps
        .protocol
        .push_service
        .queued_publish_jobs()
        .await
        .expect("queued push publish jobs");
    assert_eq!(queued_publish_jobs.len(), 1);
    assert_eq!(queued_publish_jobs[0].node(), node.node());
    state
        .deps
        .protocol
        .push_service
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain push publish jobs");
    let attempts = state
        .deps
        .protocol
        .push_service
        .delivery_attempts_for_node(node.node())
        .await
        .expect("delivery attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].device_id(), "web-1");
    assert!(!attempts[0].item_id().is_empty());
    let pubsub_node = state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&push_service_jid, node.node())
        .await
        .expect("pubsub node lookup")
        .expect("xep-0060 push node");
    assert_eq!(
        pubsub_node.config.access_model,
        waddle_xmpp::pubsub::AccessModel::Whitelist
    );
    assert_eq!(
        state
            .deps
            .protocol
            .pubsub_storage
            .get_affiliation(&push_service_jid, node.node(), &recipient)
            .await
            .expect("pubsub affiliation"),
        waddle_xmpp::pubsub::Affiliation::PublishOnly
    );
    let stored_items = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(
            &push_service_jid,
            node.node(),
            Some(1),
            &[attempts[0].item_id().to_string()],
        )
        .await
        .expect("stored pubsub push item");
    assert_eq!(stored_items.len(), 1);
    let stored_payload: Element = stored_items[0]
        .payload_xml
        .as_ref()
        .expect("notification payload")
        .parse()
        .expect("notification payload xml");
    assert!(stored_payload.is("notification", waddle_xmpp::xep::xep0357::NS_PUSH));
    let summary = stored_payload
        .children()
        .find(|child| child.is("x", waddle_xmpp::xep::NS_DATA_FORMS))
        .expect("xep-0357 summary form");
    assert!(summary.children().any(|field| {
        field.is("field", waddle_xmpp::xep::NS_DATA_FORMS)
            && field.attr("var") == Some("FORM_TYPE")
            && field.children().any(|value| {
                value.is("value", waddle_xmpp::xep::NS_DATA_FORMS)
                    && value.text() == "urn:xmpp:push:summary"
            })
    }));
    assert!(summary.children().any(|field| {
        field.is("field", waddle_xmpp::xep::NS_DATA_FORMS)
            && field.attr("var") == Some("message-count")
            && field.children().any(|value| {
                value.is("value", waddle_xmpp::xep::NS_DATA_FORMS) && value.text() == "1"
            })
    }));
    assert!(stored_payload.children().any(|child| child.is(
        "context",
        crate::notification_outbox::WADDLE_PUSH_CONTEXT_NS
    )));

    let registrations = state
        .deps
        .protocol
        .push_store
        .get_for_user(&recipient.to_string())
        .await
        .expect("registrations");
    assert!(registrations.iter().any(|registration| {
        registration.service_jid == "push.example.com"
            && registration.node.as_deref() == Some(node.node())
    }));
    assert!(registrations.iter().any(|registration| {
        registration.service_jid == "push-provider.example.com"
            && registration.node.as_deref() == Some("external-web-node")
    }));
}

#[tokio::test]
async fn queue_offline_delivery_persists_candidate_before_xep0357_registration_exists() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id(
        "offline-push-registration-late".to_string(),
    ));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "candidate first".to_string(),
    );
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-offline-push-registration-late",
        jid::Jid::from(recipient.clone()),
    );
    store_committed_dm_archive_for_notification(&state, &recipient, &archive_stanza_id, &message)
        .await;
    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("late first-party push registration");
    project_direct_unread_for_notification(
        state.as_ref(),
        &recipient,
        &sender,
        "archive-offline-push-registration-late",
    )
    .await;

    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(outbox_jobs.len(), 1);
    assert_eq!(outbox_jobs[0].conversation_jid(), &sender);
    assert_eq!(outbox_jobs[0].node().as_str(), node.node());
}

#[tokio::test]
async fn queue_offline_delivery_skips_xep0357_when_committed_mam_row_is_missing() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id("offline-push-no-mam".to_string()));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "do not push without committed archive".to_string(),
    );
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-offline-no-mam",
        jid::Jid::from(recipient.clone()),
    );
    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert!(outbox_jobs.is_empty());
    let unoutboxed = state
        .deps
        .protocol
        .pending_delivery_storage
        .list_unoutboxed_archived(16)
        .await
        .expect("unoutboxed archived rows");
    assert!(
        unoutboxed.is_empty(),
        "missing committed MAM row is a completed no-push notification decision"
    );
}

/// XEP-0492 enforcement matrix harness — drives the DM offline-delivery
/// push pipeline with a typed `(NotificationLevel, is_mention)` pair and
/// returns the count of durable XEP-0357 outbox jobs that escaped the gate.
///
/// `0` ⇒ gate suppressed the push candidate. `1` ⇒ gate created a
/// durable PubSub publish job for the recipient's registered Push Service.
async fn drive_xep0492_direct_chat_push_gate(
    level: waddle_xmpp::xep::NotificationLevel,
    is_mention: bool,
    message_id: &str,
) -> usize {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");
    state
        .deps
        .protocol
        .notification_settings_projection
        .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind: crate::notification_settings_projection::ConversationKind::Direct,
            mode: level,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: crate::time::now_ms(),
            source: crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: sender.clone(),
        })
        .await
        .expect("xep-0492 projection");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id(message_id.to_string()));
    message.type_ = XmppMessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "hello bob".to_string());
    if is_mention {
        // XEP-0513 explicit mention naming the recipient. This is the
        // sole signal the gate consults — the `<body/>` text itself is
        // not parsed for at-mention substrings (XEP-0513 §3 requires
        // explicit `<mention/>` payloads for machine-detectable
        // mentions).
        let mention = waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(recipient.clone()),
        );
        message.payloads.push(mention);
    }

    let deps = build_interpret_deps(state.as_ref(), None);
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        format!("archive-{message_id}"),
        jid::Jid::from(recipient.clone()),
    );
    store_committed_dm_archive_for_notification(&state, &recipient, &archive_stanza_id, &message)
        .await;
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    drain_notification_candidates_for_test(state.as_ref()).await;
    state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs")
        .len()
}

#[tokio::test]
async fn xep0492_direct_chat_always_delivers_push_without_mention() {
    let attempts = drive_xep0492_direct_chat_push_gate(
        waddle_xmpp::xep::NotificationLevel::Always,
        false,
        "xep0492-always-no-mention",
    )
    .await;
    assert_eq!(
        attempts, 1,
        "XEP-0492 <always/> MUST deliver push for every DM regardless of mention"
    );
}

#[tokio::test]
async fn xep0492_direct_chat_always_delivers_push_with_mention() {
    let attempts = drive_xep0492_direct_chat_push_gate(
        waddle_xmpp::xep::NotificationLevel::Always,
        true,
        "xep0492-always-with-mention",
    )
    .await;
    assert_eq!(
        attempts, 1,
        "XEP-0492 <always/> MUST deliver push for mentions too"
    );
}

#[tokio::test]
async fn xep0492_direct_chat_on_mention_suppresses_push_without_mention() {
    let attempts = drive_xep0492_direct_chat_push_gate(
        waddle_xmpp::xep::NotificationLevel::OnMention,
        false,
        "xep0492-on-mention-no-mention",
    )
    .await;
    assert_eq!(
        attempts, 0,
        "XEP-0492 <on-mention/> MUST suppress push when the recipient is not mentioned"
    );
}

#[tokio::test]
async fn xep0492_direct_chat_on_mention_delivers_push_with_mention() {
    let attempts = drive_xep0492_direct_chat_push_gate(
        waddle_xmpp::xep::NotificationLevel::OnMention,
        true,
        "xep0492-on-mention-with-mention",
    )
    .await;
    assert_eq!(
        attempts, 1,
        "XEP-0492 <on-mention/> MUST deliver push when XEP-0513 explicit mention names the recipient"
    );
}

#[tokio::test]
async fn xep0492_direct_chat_never_suppresses_push_with_mention() {
    let attempts = drive_xep0492_direct_chat_push_gate(
        waddle_xmpp::xep::NotificationLevel::Never,
        true,
        "xep0492-never-with-mention",
    )
    .await;
    assert_eq!(
        attempts, 0,
        "XEP-0492 <never/> MUST suppress push even when the recipient is mentioned"
    );
}

/// Compliance-shape probe — drives DM emission and returns the
/// post-emission `notification_candidates` row count and outbox job
/// count.
///
/// The T0 emission gate asserts on the *row count*: a suppressed
/// XEP-0492 outcome MUST leave zero rows behind (not "one row marked
/// outboxed"). The outbox-job count is reported alongside so the
/// caller can assert push-output equivalence — the previous T1-only
/// design preserved push output but persisted an audit row; this
/// shape preserves both push output AND row-zero on suppression.
async fn drive_xep0492_direct_chat_emission_shape(
    level: waddle_xmpp::xep::NotificationLevel,
    is_mention: bool,
    message_id: &str,
) -> (i64, usize) {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");
    state
        .deps
        .protocol
        .notification_settings_projection
        .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind: crate::notification_settings_projection::ConversationKind::Direct,
            mode: level,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: crate::time::now_ms(),
            source: crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: sender.clone(),
        })
        .await
        .expect("xep-0492 projection");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id(message_id.to_string()));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang(String::new()),
        "hello bob".to_string(),
    );
    if is_mention {
        let mention = waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::jid(recipient.clone()),
        );
        message.payloads.push(mention);
    }

    let deps = build_interpret_deps(state.as_ref(), None);
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        format!("archive-{message_id}"),
        jid::Jid::from(recipient.clone()),
    );
    store_committed_dm_archive_for_notification(&state, &recipient, &archive_stanza_id, &message)
        .await;
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    // Read the candidate count BEFORE draining. The T0 gate must
    // suppress the insert outright; if a row exists here the
    // compliance rule is violated regardless of what the drain
    // subsequently does with it.
    let candidate_count = state
        .deps
        .protocol
        .notification_outbox
        .count_all_candidates()
        .await
        .expect("count notification candidates");
    drain_notification_candidates_for_test(state.as_ref()).await;
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs")
        .len();
    (candidate_count, outbox_jobs)
}

/// Compliance regression: XEP-0492 `<never/>` MUST leave no row at all
/// in `notification_candidates` — not even one marked outboxed.
///
/// The previous T1-only design persisted every DM candidate at T0 and
/// then marked it outboxed at T1 without enqueuing a job; the row was
/// still observable in the table. Compliance review rejected that
/// shape — the row itself is leakage of recipient state to the user-
/// server schema. The T0 emission gate must short-circuit the insert.
#[tokio::test]
async fn xep0492_direct_chat_never_persists_no_candidate_row_at_all() {
    let (candidates, jobs) = drive_xep0492_direct_chat_emission_shape(
        waddle_xmpp::xep::NotificationLevel::Never,
        false,
        "xep0492-never-no-row",
    )
    .await;
    assert_eq!(
        candidates, 0,
        "XEP-0492 <never/> MUST NOT persist a notification_candidates row \
         (compliance: no audit trail for suppressed candidates)"
    );
    assert_eq!(jobs, 0, "XEP-0492 <never/> MUST NOT enqueue an outbox job");
}

/// Compliance regression: XEP-0492 `<on-mention/>` with a non-mention
/// DM MUST leave no row at all in `notification_candidates`.
#[tokio::test]
async fn xep0492_direct_chat_on_mention_without_mention_persists_no_candidate_row() {
    let (candidates, jobs) = drive_xep0492_direct_chat_emission_shape(
        waddle_xmpp::xep::NotificationLevel::OnMention,
        false,
        "xep0492-on-mention-no-mention-no-row",
    )
    .await;
    assert_eq!(
        candidates, 0,
        "XEP-0492 <on-mention/> with no XEP-0513 mention MUST NOT persist a candidate row"
    );
    assert_eq!(
        jobs, 0,
        "XEP-0492 <on-mention/> with no mention MUST NOT enqueue a job"
    );
}

/// Compliance regression: XEP-0492 `<on-mention/>` with an explicit
/// XEP-0513 mention naming the recipient DOES produce a candidate row
/// (and an outbox job). This is the positive side of the compliance
/// rule — suppression only zeroes rows when the evaluator says
/// `Suppressed`.
#[tokio::test]
async fn xep0492_direct_chat_on_mention_with_mention_persists_candidate_row() {
    let (candidates, jobs) = drive_xep0492_direct_chat_emission_shape(
        waddle_xmpp::xep::NotificationLevel::OnMention,
        true,
        "xep0492-on-mention-with-mention-row",
    )
    .await;
    assert_eq!(
        candidates, 1,
        "XEP-0492 <on-mention/> with XEP-0513 mention MUST persist exactly one candidate row"
    );
    assert_eq!(
        jobs, 1,
        "XEP-0492 <on-mention/> with mention MUST enqueue exactly one outbox job"
    );
}

/// Compliance regression: XEP-0492 `<always/>` (default for DMs) MUST
/// persist a candidate row and enqueue a job regardless of mention
/// state. This guards against the T0 gate over-suppressing.
#[tokio::test]
async fn xep0492_direct_chat_always_persists_candidate_row_without_mention() {
    let (candidates, jobs) = drive_xep0492_direct_chat_emission_shape(
        waddle_xmpp::xep::NotificationLevel::Always,
        false,
        "xep0492-always-no-mention-row",
    )
    .await;
    assert_eq!(
        candidates, 1,
        "XEP-0492 <always/> MUST persist a candidate row for every DM"
    );
    assert_eq!(
        jobs, 1,
        "XEP-0492 <always/> MUST enqueue exactly one outbox job"
    );
}

#[tokio::test]
async fn groupchat_personal_mention_pushes_affiliated_non_live_member() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let recipient: BareJid = "charlie@example.com".parse().expect("recipient");
    register_first_party_push_for_test(state.as_ref(), &recipient, "charlie-web").await;
    let room_jid: BareJid = "personal-mention@muc.example.com"
        .parse()
        .expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "personal-mention".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(ChangeAffiliation {
            jid: recipient.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate recipient");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "groupchat-personal-mention-push".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "charlie, take a look".to_string(),
    );
    message
        .payloads
        .push(waddle_xmpp::xep::build_reference_element(
            &waddle_xmpp::xep::Reference::mention(format!("xmpp:{recipient}")),
        ));

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "valid groupchat mention should not return an error: {responses:?}"
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &recipient);
    assert_eq!(jobs[0].conversation_jid(), &room_jid);
    assert_eq!(
        jobs[0].sender_jid().to_string(),
        "personal-mention@muc.example.com/alice"
    );
    assert_eq!(
        jobs[0].class(),
        crate::notification_outbox::NotificationClass::PersonalMention
    );
}

#[tokio::test]
async fn groupchat_xep0513_occupant_id_mention_pushes_affiliated_member() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let recipient: BareJid = "charlie@example.com".parse().expect("recipient");
    register_first_party_push_for_test(state.as_ref(), &recipient, "charlie-occupant-id-web").await;
    let room_jid: BareJid = "occupant-id-mention@muc.example.com"
        .parse()
        .expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "occupant-id-mention".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(ChangeAffiliation {
            jid: recipient.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate recipient");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");

    let occupant_id = waddle_xmpp::xep::generate_occupant_id(
        &recipient,
        &room_jid,
        &state.deps.occupant_id_secret,
    );
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "groupchat-occupant-id-mention-push".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "@charlie, take a look".to_string(),
    );
    message
        .payloads
        .push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::occupant_id(occupant_id.as_str()),
        ));

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "valid XEP-0513 occupant-id mention should not return an error: {responses:?}"
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &recipient);
    assert_eq!(
        jobs[0].class(),
        crate::notification_outbox::NotificationClass::PersonalMention
    );
}

#[tokio::test]
async fn groupchat_xep0492_never_suppresses_personal_mentions_and_plain_messages() {
    async fn drive(message_id: &str, mention: bool) -> usize {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_session(state.as_ref(), "alice").await;
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let recipient: BareJid = "charlie-never@example.com".parse().expect("recipient");
        register_first_party_push_for_test(state.as_ref(), &recipient, "charlie-never-web").await;
        let room_jid: BareJid = format!("{message_id}@muc.example.com")
            .parse()
            .expect("room jid");
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room_jid,
            RoomConfig {
                members_only: false,
                ..RoomConfig::default()
            },
            "space".to_string(),
            message_id.to_string(),
        )
        .await
        .expect("create room")
        .actor_ref;
        room_actor
            .ask(ChangeAffiliation {
                jid: recipient.clone(),
                affiliation: Affiliation::Member,
            })
            .await
            .expect("affiliate recipient");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: alice_jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: current_admission_revision(&room_actor).await,
            })
            .await
            .expect("join alice");
        state
            .deps
            .protocol
            .notification_settings_projection
            .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: recipient.clone(),
                conversation_jid: room_jid.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::PublicGroup,
                mode: waddle_xmpp::xep::NotificationLevel::Never,
                rich_payload_opt_in: false,
                source_version: 1,
                updated_at_ms: crate::time::now_ms(),
                source:
                    crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: room_jid.clone(),
            })
            .await
            .expect("xep-0492 projection");

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
        message.id = Some(xmpp_parsers::message::Id(message_id.to_string()));
        message.type_ = XmppMessageType::Groupchat;
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "never means never".to_string(),
        );
        if mention {
            message
                .payloads
                .push(waddle_xmpp::xep::build_reference_element(
                    &waddle_xmpp::xep::Reference::mention(format!("xmpp:{recipient}")),
                ));
        }

        let responses =
            handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message)
                .await;
        assert!(
            responses.is_empty(),
            "valid groupchat message should not return an error: {responses:?}"
        );

        drain_notification_candidates_for_test(state.as_ref()).await;
        state
            .deps
            .protocol
            .notification_outbox
            .pending_outbox_jobs()
            .await
            .expect("notification outbox jobs")
            .len()
    }

    assert_eq!(
        drive("group-never-mention", true).await,
        0,
        "XEP-0492 <never/> MUST suppress mention-derived groupchat push candidates"
    );
    assert_eq!(
        drive("group-never-plain", false).await,
        0,
        "XEP-0492 <never/> MUST suppress plain groupchat push candidates"
    );
}

#[tokio::test]
async fn groupchat_notification_recovery_retries_committed_inbox_projection() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "charlie-recover@example.com".parse().expect("recipient");
    register_first_party_push_for_test(state.as_ref(), &recipient, "charlie-recover-web").await;
    let room_jid: BareJid = "groupchat-recovery@muc.example.com"
        .parse()
        .expect("room jid");
    // The T1 push evaluator now defers candidates when the room
    // actor is not live (no durable T1 projection of MUC config in
    // slice 1), so the recovery test must spin up the room actor
    // before draining — otherwise the candidate row is deferred
    // with policy_error_count = 1 instead of becoming a push job.
    let _room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "groupchat-recovery".to_string(),
    )
    .await
    .expect("create recovery room")
    .actor_ref;
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "groupchat-recovery-archive",
        jid::Jid::from(room_jid.clone()),
    );
    let sender_jid: jid::Jid = "groupchat-recovery@muc.example.com/alice"
        .parse()
        .expect("sender jid");
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(sender_jid.clone());
    message.id = Some(xmpp_parsers::message::Id(
        "groupchat-recovery-wire".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "charlie, this should recover".to_string(),
    );
    message
        .payloads
        .push(waddle_xmpp::xep::build_reference_element(
            &waddle_xmpp::xep::Reference::mention(format!("xmpp:{recipient}")),
        ));
    waddle_xmpp_core::xep0359::add_stanza_id(&mut message, &archive_stanza_id);
    store_committed_dm_archive_for_notification(&state, &room_jid, &archive_stanza_id, &message)
        .await;

    state
        .deps
        .protocol
        .inbox_storage
        .insert_groupchat_notification_recovery(
            waddle_xmpp::inbox::storage::GroupchatNotificationRecovery {
                key: waddle_xmpp::inbox::storage::GroupchatNotificationRecoveryKey {
                    recipient: recipient.clone(),
                    room: room_jid.clone(),
                    thread_id: None,
                    archive_stanza_id,
                },
                sender_jid,
                is_live_occupant: false,
                room_members_only: false,
                sender_can_broadcast_channel_mention: false,
                created_at_ms: crate::time::now_ms(),
            },
        )
        .await
        .expect("insert recovery item");

    assert_eq!(
        crate::server::routes::interpret::reconcile_groupchat_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        1,
        "recovery should complete the durable item after enqueueing the candidate"
    );
    assert!(
        state
            .deps
            .protocol
            .inbox_storage
            .list_pending_groupchat_notification_recoveries(16)
            .await
            .expect("list recovery")
            .is_empty(),
        "completed groupchat recovery items must not be retried"
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &recipient);
    assert_eq!(jobs[0].conversation_jid(), &room_jid);
    assert_eq!(
        jobs[0].class(),
        crate::notification_outbox::NotificationClass::PersonalMention
    );
}

#[tokio::test]
async fn groupchat_public_default_suppresses_plain_push_until_always() {
    async fn drive(level: Option<waddle_xmpp::xep::NotificationLevel>, message_id: &str) -> usize {
        let state = create_test_websocket_state().await;
        let alice_session = create_test_session(state.as_ref(), "alice").await;
        let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
            .parse()
            .expect("alice jid");
        let recipient: BareJid = "charlie@example.com".parse().expect("recipient");
        register_first_party_push_for_test(state.as_ref(), &recipient, "charlie-web").await;
        let room_jid: BareJid = format!("{message_id}@muc.example.com")
            .parse()
            .expect("room jid");
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room_jid,
            RoomConfig {
                members_only: false,
                ..RoomConfig::default()
            },
            "space".to_string(),
            message_id.to_string(),
        )
        .await
        .expect("create room")
        .actor_ref;
        room_actor
            .ask(ChangeAffiliation {
                jid: recipient.clone(),
                affiliation: Affiliation::Member,
            })
            .await
            .expect("affiliate recipient");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: alice_jid.clone(),
                nick: "alice".to_string(),
                affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
                local_domain: "example.com".to_string(),
                admission_revision: current_admission_revision(&room_actor).await,
            })
            .await
            .expect("join alice");
        if let Some(level) = level {
            state
                .deps
                .protocol
                .notification_settings_projection
                .upsert(
                    &crate::notification_settings_projection::NotificationSettingsProjection {
                        owner_bare_jid: recipient.clone(),
                        conversation_jid: room_jid.clone(),
                        conversation_kind:
                            crate::notification_settings_projection::ConversationKind::PublicGroup,
                        mode: level,
                        rich_payload_opt_in: false,
                        source_version: 1,
                        updated_at_ms: crate::time::now_ms(),
                        source:
                            crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                        source_item_jid: room_jid.clone(),
                    },
                )
                .await
                .expect("xep-0492 projection");
        }

        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
        message.id = Some(xmpp_parsers::message::Id(message_id.to_string()));
        message.type_ = XmppMessageType::Groupchat;
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "plain public-room message".to_string(),
        );
        let responses =
            handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message)
                .await;
        assert!(
            responses.is_empty(),
            "valid groupchat message should not return an error: {responses:?}"
        );

        drain_notification_candidates_for_test(state.as_ref()).await;
        state
            .deps
            .protocol
            .notification_outbox
            .pending_outbox_jobs()
            .await
            .expect("notification outbox jobs")
            .len()
    }

    assert_eq!(
        drive(None, "public-default-plain").await,
        0,
        "public group default is XEP-0492 <on-mention/> and must suppress plain messages"
    );
    assert_eq!(
        drive(
            Some(waddle_xmpp::xep::NotificationLevel::Always),
            "public-always-plain"
        )
        .await,
        1,
        "XEP-0492 <always/> must push plain public-room messages"
    );
}

#[tokio::test]
async fn groupchat_channel_mention_for_foreign_room_does_not_ping_current_room() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let recipient: BareJid = "charlie@example.com".parse().expect("recipient");
    register_first_party_push_for_test(state.as_ref(), &recipient, "charlie-foreign-channel-web")
        .await;
    let room_jid: BareJid = "current-channel-uri@muc.example.com"
        .parse()
        .expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "current-channel-uri".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(ChangeAffiliation {
            jid: recipient,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate recipient");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");

    let mut mention = waddle_xmpp::xep::ExplicitMention::channel();
    mention.uri = Some("xmpp:other-channel@muc.example.com".to_string());
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid)));
    message.id = Some(xmpp_parsers::message::Id(
        "foreign-channel-mention".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "not this room".to_string(),
    );
    message
        .payloads
        .push(waddle_xmpp::xep::build_mention_element(&mention));

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "valid foreign-room channel mention should not return an error: {responses:?}"
    );
    // Post-#526 slice 1: foreign-room mentions are not personal/channel
    // mentions for current-room members, so T0 classifies the candidate
    // as NotifyAll. The XEP-0492 public-group default (`OnMention`)
    // then suppresses publication at T1.
    drain_notification_candidates_for_test(state.as_ref()).await;
    assert!(
        state
            .deps
            .protocol
            .notification_outbox
            .pending_outbox_jobs()
            .await
            .expect("notification outbox jobs")
            .is_empty(),
        "a XEP-0513 #channel mention with a foreign room URI must not notify current-room members"
    );
}

#[tokio::test]
async fn groupchat_active_channel_mention_pushes_live_occupants_only() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let bob_bare = bob_jid.to_bare();
    let charlie: BareJid = "charlie@example.com".parse().expect("recipient");
    register_first_party_push_for_test(state.as_ref(), &bob_bare, "bob-web").await;
    register_first_party_push_for_test(state.as_ref(), &charlie, "charlie-web").await;
    let room_jid: BareJid = "active-channel@muc.example.com".parse().expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "active-channel".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(ChangeAffiliation {
            jid: bob_bare.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate live recipient");
    room_actor
        .ask(ChangeAffiliation {
            jid: charlie,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate non-live recipient");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            // XEP-0513 §"Multi-User Chats Permissions" §304: alice
            // MUST hold the minimum-role-required (server default
            // `mentions#channel = moderators`) to broadcast a channel
            // mention. `Affiliation::Admin` derives `Role::Moderator`
            // via `room_affiliations`, satisfying the gate. The point
            // of this test is the `<active/>` recipient filter; the
            // sender's role is fixture scaffolding.
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Admin),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("active-channel-push".to_string()));
    message.type_ = XmppMessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "heads up".to_string());
    message
        .payloads
        .push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::active_channel(),
        ));

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "valid active channel mention should not return an error: {responses:?}"
    );
    // Slice 2b: XEP-0513 `<active/>` filter consults the
    // `notification_activity` projection at T1. Bob is "live"
    // (joined the room) but the test's join doesn't currently
    // wire into the projection's ingestion path — seed bob's
    // activity for the room directly so the filter passes.
    seed_notification_activity_for_test(state.as_ref(), &bob_bare, &room_jid).await;
    // Post-#526 slice 1: T0 emits one candidate per affiliated recipient
    // (bob = live, charlie = non-live), and T1 suppresses charlie's
    // candidate via the XEP-0492 public-group `OnMention` default once
    // the non-live `NotifyAll` class is classified at T0.
    drain_notification_candidates_for_test(state.as_ref()).await;
    let jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recipient_bare_jid(), &bob_bare);
    assert_eq!(jobs[0].conversation_jid(), &room_jid);
    assert_eq!(
        jobs[0].class(),
        crate::notification_outbox::NotificationClass::ActiveChannelMention
    );
}

#[tokio::test]
async fn groupchat_active_channel_mention_does_not_expand_to_live_unaffiliated_occupants() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = "bob-live-only@example.com/web".parse().expect("bob jid");
    let bob_bare = bob_jid.to_bare();
    register_first_party_push_for_test(state.as_ref(), &bob_bare, "bob-live-only-web").await;
    let room_jid: BareJid = "active-channel-live-not-durable@muc.example.com"
        .parse()
        .expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: false,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "active-channel-live-not-durable".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            // XEP-0513 §"Multi-User Chats Permissions" §304: this test
            // asserts that live-only-not-affiliated occupants don't get
            // pushed for `<active/>` channel mentions. The sender's
            // role MUST be Moderator-equivalent so the channel mention
            // actually classifies as `ActiveChannelMention` — otherwise
            // the assertion passes for the wrong reason (downgrade to
            // `NotifyAll` + no durable recipient = empty jobs).
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Admin),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            affiliation_grant: JoinAffiliationGrant::Unaffiliated,
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join bob without durable affiliation");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid)));
    message.id = Some(xmpp_parsers::message::Id(
        "active-channel-live-not-durable-push".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "heads up".to_string());
    message
        .payloads
        .push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::active_channel(),
        ));

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "valid active channel mention should not return an error: {responses:?}"
    );
    let drained = drain_notification_candidates_for_test(state.as_ref()).await;
    let jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(
        drained, 0,
        "live occupancy alone must not expand the durable groupchat push recipient set: {jobs:?}"
    );
    assert!(jobs.is_empty());
}

#[tokio::test]
async fn groupchat_active_channel_mention_preserves_notify_all_for_non_live_always_members() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = "bob-always@example.com/web".parse().expect("bob jid");
    let bob_bare = bob_jid.to_bare();
    let charlie: BareJid = "charlie-always@example.com".parse().expect("recipient");
    register_first_party_push_for_test(state.as_ref(), &bob_bare, "bob-always-web").await;
    register_first_party_push_for_test(state.as_ref(), &charlie, "charlie-always-web").await;
    let room_jid: BareJid = "active-channel-always@muc.example.com"
        .parse()
        .expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "active-channel-always".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(ChangeAffiliation {
            jid: bob_bare.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate live recipient");
    room_actor
        .ask(ChangeAffiliation {
            jid: charlie.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("affiliate non-live recipient");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            // XEP-0513 §"Multi-User Chats Permissions" §304: alice
            // needs role >= moderator to broadcast a channel mention
            // under the server's default `mentions#channel = moderators`
            // policy. Test focus is the live-occupancy + Always
            // members-only path; sender role is scaffolding.
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Admin),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "active-channel-always-push".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "heads up".to_string());
    message
        .payloads
        .push(waddle_xmpp::xep::build_mention_element(
            &waddle_xmpp::xep::ExplicitMention::active_channel(),
        ));

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "valid active channel mention should not return an error: {responses:?}"
    );
    // Slice 2b: seed bob's activity for the room so the XEP-0513
    // `<active/>` filter passes. The members-only `Always` default
    // then preserves the `NotifyAll` job for charlie regardless.
    seed_notification_activity_for_test(state.as_ref(), &bob_bare, &room_jid).await;
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        2
    );
    let jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(jobs.len(), 2);
    let bob_job = jobs
        .iter()
        .find(|job| job.recipient_bare_jid() == &bob_bare)
        .expect("bob active-channel job");
    assert_eq!(
        bob_job.class(),
        crate::notification_outbox::NotificationClass::ActiveChannelMention
    );
    let charlie_job = jobs
        .iter()
        .find(|job| job.recipient_bare_jid() == &charlie)
        .expect("charlie notify-all job");
    assert_eq!(
        charlie_job.class(),
        crate::notification_outbox::NotificationClass::NotifyAll
    );
}

#[tokio::test]
async fn queue_offline_delivery_suppresses_xep0357_when_xep0492_direct_chat_is_never() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");
    state
        .deps
        .protocol
        .notification_settings_projection
        .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind: crate::notification_settings_projection::ConversationKind::Direct,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: crate::time::now_ms(),
            source: crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: sender.clone(),
        })
        .await
        .expect("xep-0492 projection");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id(
        "offline-push-muted-1".to_string(),
    ));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "do not push".to_string(),
    );
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-offline-muted-1",
        jid::Jid::from(recipient.clone()),
    );
    store_committed_dm_archive_for_notification(&state, &recipient, &archive_stanza_id, &message)
        .await;
    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    let attempts = state
        .deps
        .protocol
        .push_service
        .delivery_attempts_for_node(node.node())
        .await
        .expect("delivery attempts");
    assert!(attempts.is_empty());
    let queued = state
        .deps
        .protocol
        .push_service
        .queued_publish_jobs()
        .await
        .expect("queued jobs");
    assert!(queued.is_empty());
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert!(outbox_jobs.is_empty());
}

/// Wire-level regression for slice 2a's storage-preservation contract:
/// when push is suppressed by XEP-0492 `<never/>` at T0, the upstream
/// artifacts that hold the message MUST remain intact — XEP-0313 MAM
/// archive row, XEP-0430 inbox projection, and XEP-0160 pending
/// delivery queue entry. The notification outbox layer touches only
/// `notification_candidates` / `notification_outbox`; this test pins
/// the contract end-to-end through `interpret(QueueOfflineDelivery)`
/// so a future refactor that bundles upstream + push writes into one
/// transaction (and accidentally rolls back the MAM row on push
/// suppression) is caught immediately.
///
/// Per the brief: this single comprehensive ws-integration test
/// stands in for individual per-XEP smoke tests across the
/// `xep0313_mam_integration`, `xep0430_inbox_ws`, and
/// `xep0160_offline_message_handling` files. Those live in three
/// different crates / fixture styles; one wire-level shot is the
/// economical place to assert the joint invariant. Per-XEP unit
/// coverage of the audit + suppression decisions lives in
/// `notification_outbox::tests` (slice 2a storage-preservation
/// suite).
#[tokio::test]
async fn xep0357_suppression_preserves_mam_inbox_pending_delivery_and_audit() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    register_first_party_push_for_test(state.as_ref(), &recipient, "web-1").await;

    // Recipient muted this DM conversation — XEP-0492 `<never/>` is a
    // T0 compliance suppressor.
    state
        .deps
        .protocol
        .notification_settings_projection
        .upsert(&crate::notification_settings_projection::NotificationSettingsProjection {
            owner_bare_jid: recipient.clone(),
            conversation_jid: sender.clone(),
            conversation_kind: crate::notification_settings_projection::ConversationKind::Direct,
            mode: waddle_xmpp::xep::NotificationLevel::Never,
            rich_payload_opt_in: false,
            source_version: 1,
            updated_at_ms: crate::time::now_ms(),
            source: crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: sender.clone(),
        })
        .await
        .expect("xep-0492 projection");

    // Build the inbound DM exactly the same way the offline path sees
    // it: full sender JID, body, deterministic id.
    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id("storage-preserve-1".to_string()));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang(String::new()),
        "must-not-rollback".to_string(),
    );
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-storage-preserve-1",
        jid::Jid::from(recipient.clone()),
    );

    // Seed XEP-0313 MAM row, XEP-0430 inbox projection, and XEP-0160
    // pending_delivery row BEFORE the offline-delivery interpret pass.
    // These represent every upstream artifact the candidate-emission
    // code path must NOT roll back.
    store_committed_dm_archive_for_notification(&state, &recipient, &archive_stanza_id, &message)
        .await;
    project_direct_unread_for_notification(
        state.as_ref(),
        &recipient,
        &sender,
        "archive-storage-preserve-1",
    )
    .await;
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(
                archive_stanza_id.clone(),
            ),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("seed pending_delivery row");

    // Snapshot upstream state for the post-emission diff.
    let mam_before = state
        .deps
        .protocol
        .mam_storage
        .get_message_by_stanza_id(&recipient, &archive_stanza_id.id)
        .await
        .expect("mam lookup before");
    assert!(
        mam_before.is_some(),
        "seed MAM row must be queryable before emission",
    );
    let pending_before = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("pending list before");
    assert_eq!(
        pending_before.len(),
        1,
        "seed pending_delivery row must be present before emission",
    );
    let inbox_before = state
        .deps
        .protocol
        .inbox_storage
        .list(&recipient)
        .await
        .expect("inbox list before");
    assert_eq!(
        inbox_before.len(),
        1,
        "seed inbox row must be present before emission",
    );

    // Drive the T0 emission path through the interpret loop just like
    // the live DM handler does.
    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(
                archive_stanza_id.clone(),
            ),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    // Push surface: no candidate row, no outbox job, no provider
    // delivery attempt.
    let candidate_count = state
        .deps
        .protocol
        .notification_outbox
        .count_all_candidates()
        .await
        .expect("count candidates");
    assert_eq!(
        candidate_count, 0,
        "T0 <never/> suppression MUST NOT persist a candidate row",
    );
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("pending outbox jobs");
    assert!(
        outbox_jobs.is_empty(),
        "T0 <never/> suppression MUST NOT enqueue an outbox job",
    );

    // Upstream invariants: MAM row, inbox entry, pending_delivery row
    // are all byte-identical to the pre-emission snapshot.
    let mam_after = state
        .deps
        .protocol
        .mam_storage
        .get_message_by_stanza_id(&recipient, &archive_stanza_id.id)
        .await
        .expect("mam lookup after");
    assert!(
        mam_after.is_some(),
        "XEP-0313 MAM row MUST survive push suppression",
    );
    assert_eq!(
        mam_before.as_ref().map(|m| &m.id),
        mam_after.as_ref().map(|m| &m.id),
        "MAM archive id MUST be unchanged across push suppression",
    );

    let pending_after = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("pending list after");
    let seed_id = pending_before[0].id.as_str();
    assert!(
        pending_after.iter().any(|row| row.id.as_str() == seed_id),
        "XEP-0160 pending_delivery row seeded before emission MUST survive push suppression; \
         after={pending_after:?}",
    );

    let inbox_after = state
        .deps
        .protocol
        .inbox_storage
        .list(&recipient)
        .await
        .expect("inbox list after");
    let seed_partner = &inbox_before[0].partner;
    assert!(
        inbox_after
            .iter()
            .any(|entry| &entry.partner == seed_partner),
        "XEP-0430 inbox entry for the seeded partner MUST survive push suppression; \
         after={inbox_after:?}",
    );
}

#[tokio::test]
async fn queue_offline_delivery_suppresses_xep0357_for_transient_no_permanent_store_payloads() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");

    let mut message =
        xmpp_parsers::message::Message::new(Some("bob@example.com".parse().expect("to jid")));
    message.from = Some("alice@example.com/web".parse().expect("from jid"));
    message.id = Some(xmpp_parsers::message::Id(
        "offline-push-transient-1".to_string(),
    ));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "do not push transient".to_string(),
    );
    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::QueueOfflineDelivery {
            recipient: recipient.clone(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(
                message.clone(),
            )),
            original_receipt_at: chrono::Utc::now(),
            original_message: Box::new(message),
        }],
        &deps,
    )
    .await;

    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert!(outbox_jobs.is_empty());
    let queued = state
        .deps
        .protocol
        .push_service
        .queued_publish_jobs()
        .await
        .expect("queued jobs");
    assert!(queued.is_empty());
    let attempts = state
        .deps
        .protocol
        .push_service
        .delivery_attempts_for_node(node.node())
        .await
        .expect("delivery attempts");
    assert!(attempts.is_empty());
}

#[tokio::test]
async fn notification_candidate_recovery_rebuilds_from_committed_pending_delivery_and_mam() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");

    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-recovery-1",
        jid::Jid::from(recipient.clone()),
    );
    let archived = waddle_xmpp_core::mam::ArchivedMessage {
        id: archive_stanza_id.id.clone(),
        body: Some("recover me".to_string()),
        stanza_id: Some(archive_stanza_id.clone()),
        message_type: XmppMessageType::Chat,
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            "alice@example.com/web".parse().expect("sender jid"),
            jid::Jid::from(recipient.clone()),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&recipient, &archived)
        .await
        .expect("store MAM row");
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");

    let recovered = crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
        state.as_ref(),
        16,
    )
    .await;
    assert_eq!(recovered, 1);
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(outbox_jobs.len(), 1);
    assert_eq!(outbox_jobs[0].conversation_jid(), &sender);
    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        0,
        "pending_delivery marker should prevent repeated recovery of the same committed row"
    );
}

#[tokio::test]
async fn notification_candidate_recovery_preserves_sender_resource_from_archived_stanza_xml() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender_bare: BareJid = "alice@example.com".parse().expect("sender");
    let sender_full: jid::Jid = "alice@example.com/web".parse().expect("sender full jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");

    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-recovery-resource-1",
        jid::Jid::from(recipient.clone()),
    );
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
    message.from = Some(sender_full.clone());
    message.id = Some(xmpp_parsers::message::Id(
        "wire-recovery-resource-1".to_string(),
    ));
    message.type_ = XmppMessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), "recover me".to_string());
    message
        .payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            archive_stanza_id.id.as_str(),
            &archive_stanza_id.by,
        ));

    let deps = build_interpret_deps(state.as_ref(), None);
    crate::server::routes::interpret::interpret(
        vec![waddle_xmpp::protocol::OutboundEvent::ArchiveDirect {
            archive_jid: recipient.clone(),
            from: sender_bare.clone().into(),
            to: recipient.clone().into(),
            message: Box::new(message),
        }],
        &deps,
    )
    .await;
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");

    let recovered = crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
        state.as_ref(),
        16,
    )
    .await;
    assert_eq!(recovered, 1);
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        1
    );
    let outbox_jobs = state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs");
    assert_eq!(outbox_jobs.len(), 1);
    assert_eq!(outbox_jobs[0].conversation_jid(), &sender_bare);
    assert_eq!(outbox_jobs[0].sender_jids(), &[sender_full]);
}

#[tokio::test]
async fn notification_candidate_recovery_skips_full_mam_sender_when_stanza_sender_conflicts() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender_full: jid::Jid = "alice@example.com/web".parse().expect("sender full jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");

    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-recovery-full-row-mismatched-stanza-1",
        jid::Jid::from(recipient.clone()),
    );
    let mut stanza_message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
    stanza_message.from = Some("alice@example.com/phone".parse().expect("stanza sender"));
    stanza_message.id = Some(xmpp_parsers::message::Id(
        "wire-recovery-full-row-mismatched-stanza-1".to_string(),
    ));
    stanza_message.type_ = XmppMessageType::Chat;
    stanza_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "recover from full MAM row".to_string(),
    );
    let archived = waddle_xmpp_core::mam::ArchivedMessage {
        id: archive_stanza_id.id.clone(),
        body: Some("recover from full MAM row".to_string()),
        stanza_id: Some(archive_stanza_id.clone()),
        message_type: XmppMessageType::Chat,
        stanza_xml: Some(
            waddle_xmpp::parser::message_to_string(&stanza_message)
                .expect("serialize mismatched archived message"),
        ),
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            sender_full.clone(),
            jid::Jid::from(recipient.clone()),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&recipient, &archived)
        .await
        .expect("store MAM row");
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");

    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        1
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        0
    );
    assert!(state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs")
        .is_empty());
    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        0,
        "conflicted sender provenance is terminal"
    );
}

#[tokio::test]
async fn notification_candidate_recovery_skips_bare_stanza_sender_even_with_full_mam_sender() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender_bare: BareJid = "alice@example.com".parse().expect("sender");
    let sender_full: jid::Jid = "alice@example.com/web".parse().expect("sender full jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&recipient, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &recipient,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("push device");
    state
        .deps
        .protocol
        .push_service
        .register_first_party_node_for_owner(&recipient, "push.example.com", node.node(), None)
        .await
        .expect("first-party push registration");

    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-recovery-full-row-bare-stanza-1",
        jid::Jid::from(recipient.clone()),
    );
    let mut stanza_message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
    stanza_message.from = Some(jid::Jid::from(sender_bare));
    stanza_message.id = Some(xmpp_parsers::message::Id(
        "wire-recovery-full-row-bare-stanza-1".to_string(),
    ));
    stanza_message.type_ = XmppMessageType::Chat;
    stanza_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "do not recover bare stanza sender".to_string(),
    );
    let archived = waddle_xmpp_core::mam::ArchivedMessage {
        id: archive_stanza_id.id.clone(),
        body: Some("do not recover bare stanza sender".to_string()),
        stanza_id: Some(archive_stanza_id.clone()),
        message_type: XmppMessageType::Chat,
        stanza_xml: Some(
            waddle_xmpp::parser::message_to_string(&stanza_message)
                .expect("serialize bare-sender archived message"),
        ),
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            sender_full,
            jid::Jid::from(recipient.clone()),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&recipient, &archived)
        .await
        .expect("store MAM row");
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");

    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        1
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        0
    );
    assert!(state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs")
        .is_empty());
    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        0,
        "bare stanza sender conflicts with exact MAM sender provenance"
    );
}

#[tokio::test]
async fn notification_candidate_recovery_skips_bare_only_sender_provenance_terminally() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let sender: BareJid = "alice@example.com".parse().expect("sender");
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-recovery-bare-sender-1",
        jid::Jid::from(recipient.clone()),
    );
    let archived = waddle_xmpp_core::mam::ArchivedMessage {
        id: archive_stanza_id.id.clone(),
        body: Some("do not recover without resource".to_string()),
        stanza_id: Some(archive_stanza_id.clone()),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            jid::Jid::from(sender),
            jid::Jid::from(recipient.clone()),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&recipient, &archived)
        .await
        .expect("store MAM row");
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");

    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        1
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        0
    );
    assert!(state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs")
        .is_empty());
    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        0,
        "pending_delivery marker should prevent retrying terminal no-provenance rows"
    );
}

#[tokio::test]
async fn notification_candidate_recovery_skips_mismatched_stanza_sender_terminally() {
    let state = create_test_websocket_state().await;
    let recipient: BareJid = "bob@example.com".parse().expect("recipient");
    let archive_sender: BareJid = "alice@example.com".parse().expect("archive sender");
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "archive-recovery-mismatched-sender-1",
        jid::Jid::from(recipient.clone()),
    );
    let mut stanza_message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
    stanza_message.from = Some("mallory@example.com/web".parse().expect("stanza sender"));
    stanza_message.id = Some(xmpp_parsers::message::Id(
        "wire-recovery-mismatched-sender-1".to_string(),
    ));
    stanza_message.type_ = XmppMessageType::Chat;
    stanza_message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "do not recover mismatched sender".to_string(),
    );
    let archived = waddle_xmpp_core::mam::ArchivedMessage {
        id: archive_stanza_id.id.clone(),
        body: Some("do not recover mismatched sender".to_string()),
        stanza_id: Some(archive_stanza_id.clone()),
        message_type: XmppMessageType::Chat,
        stanza_xml: Some(
            waddle_xmpp::parser::message_to_string(&stanza_message)
                .expect("serialize mismatched archived message"),
        ),
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            jid::Jid::from(archive_sender),
            jid::Jid::from(recipient.clone()),
        )
    };
    state
        .deps
        .protocol
        .mam_storage
        .store_message(&recipient, &archived)
        .await
        .expect("store MAM row");
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert pending_delivery row");

    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        1
    );
    assert_eq!(
        drain_notification_candidates_for_test(state.as_ref()).await,
        0
    );
    assert!(state
        .deps
        .protocol
        .notification_outbox
        .pending_outbox_jobs()
        .await
        .expect("notification outbox jobs")
        .is_empty());
    assert_eq!(
        crate::server::routes::interpret::reconcile_xep0357_notification_candidates(
            state.as_ref(),
            16,
        )
        .await,
        0,
        "pending_delivery marker should prevent retrying terminal mismatched-provenance rows"
    );
}

#[tokio::test]
async fn handle_message_direct_rejects_client_authored_extension_envelope() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "dm-extension-spoof-1".to_string(),
    ));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "spoofed extension".to_string(),
    );
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());
    message
        .payloads
        .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

    let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("bad-request"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:test-extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("from='bob@example.com/mobile'"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to='alice@example.com/web'"),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_rejects_client_authored_inbox_payloads() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let mut waddle_push =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    waddle_push.id = Some(xmpp_parsers::message::Id("dm-inbox-spoof-1".to_string()));
    waddle_push.type_ = XmppMessageType::Headline;
    waddle_push.payloads.push(
        Element::builder("push", "urn:waddle:inbox:0")
            .append(
                Element::builder("entry", "urn:xmpp:inbox:1")
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "room@muc.example.com",
                    )
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sid-1")
                    .attr(minidom::rxml::xml_ncname!("unread").to_owned(), "99")
                    .build(),
            )
            .append(
                Element::builder("metadata", "urn:waddle:inbox:0")
                    .attr(minidom::rxml::xml_ncname!("kind").to_owned(), "muc")
                    .attr(
                        minidom::rxml::xml_ncname!("last-updated").to_owned(),
                        "1700000001",
                    )
                    .build(),
            )
            .build(),
    );

    let mut official_entry =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    official_entry.id = Some(xmpp_parsers::message::Id("dm-inbox-spoof-2".to_string()));
    official_entry.type_ = XmppMessageType::Chat;
    official_entry.payloads.push(
        Element::builder("entry", "urn:xmpp:inbox:1")
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "room@muc.example.com",
            )
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "sid-2")
            .attr(minidom::rxml::xml_ncname!("unread").to_owned(), "99")
            .build(),
    );

    for message in [waddle_push, official_entry] {
        let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("bad-request"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:waddle:inbox:0"),
            "response was {}",
            responses[0]
        );
        assert!(
            !responses[0].contains("urn:xmpp:inbox:1"),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("from='bob@example.com/mobile'"),
            "response was {}",
            responses[0]
        );
        assert!(
            responses[0].contains("to='alice@example.com/web'"),
            "response was {}",
            responses[0]
        );
    }
}

#[tokio::test]
async fn handle_message_error_with_extension_envelope_does_not_emit_error_loop() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "dm-extension-error-1".to_string(),
    ));
    message.type_ = XmppMessageType::Error;
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());

    let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

    assert!(
        responses.is_empty(),
        "message errors must not trigger another error: {responses:?}"
    );
}

#[tokio::test]
async fn handle_message_groupchat_rejects_client_authored_extension_envelope() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("sender jid");
    let room_jid: BareJid = "general@muc.example.com".parse().expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        "space".to_string(),
        "general".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join alice");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "muc-extension-spoof-1".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "spoofed extension".to_string(),
    );
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());
    message
        .payloads
        .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

    let responses =
        handle_message_for_test(state.as_ref(), &sender_jid, Some(&session), message).await;

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("bad-request"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:test-extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("from='general@muc.example.com'"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to='alice@example.com/web'"),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_groupchat_extension_envelope_preserves_non_occupant_error() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let room_jid: BareJid = "general@muc.example.com".parse().expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        "space".to_string(),
        "general".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "muc-extension-non-occupant-1".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "spoofed extension".to_string(),
    );
    message
        .payloads
        .push(Element::builder("extensions", "urn:waddle:extension:1").build());
    message
        .payloads
        .push(Element::builder("spoof", "urn:waddle:test-extension:1").build());

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;

    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("not-acceptable"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("bad-request"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        !responses[0].contains("urn:waddle:test-extension:1"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("from='general@muc.example.com'"),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to='alice@example.com/web'"),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_direct_with_sample_extension_payload_preserves_payload_for_recipient() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
    // ADR-0017 Slice 2: delivery reads the actor tree, so register into both.
    register_test_connection(state.as_ref(), &recipient_jid, recipient_tx).await;

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "dm-extension-payload-1".to_string(),
    ));
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "Repo payload already attached".to_string(),
    );
    message.payloads.push(
        Element::builder("repo", "urn:waddle:test-extension:1")
            .attr(minidom::rxml::xml_ncname!("owner").to_owned(), "rust-lang")
            .attr(minidom::rxml::xml_ncname!("name").to_owned(), "rust")
            .attr(
                minidom::rxml::xml_ncname!("url").to_owned(),
                "xmpp:example.com?extension=test",
            )
            .build(),
    );

    let responses = handle_message_for_test(state.as_ref(), &sender_jid, None, message).await;

    assert!(
        responses.is_empty(),
        "direct messages should not get a special extension echo"
    );

    // Recipient may receive an inbox push headline before the routed
    // chat message.  Drain until we find the chat stanza.
    let mut found_chat = false;
    while let Ok(routed) = recipient_rx.try_recv() {
        let routed_xml = stanza_to_xml(&routed.stanza);
        if routed_xml.contains("type='chat'") || routed_xml.contains("type='chat'") {
            assert!(
                routed_xml.contains("to='bob@example.com/mobile'")
                    || routed_xml.contains("to='bob@example.com/mobile'"),
                "routed stanza should target recipient resource: {routed_xml}"
            );
            assert!(
                routed_xml.contains("urn:waddle:test-extension:1"),
                "routed stanza should preserve extension payload: {routed_xml}"
            );
            assert_sample_payload(
                &routed_xml,
                "repo",
                "xmpp:example.com?extension=test",
                "rust-lang",
                "rust",
            );
            found_chat = true;
            break;
        }
    }
    assert!(found_chat, "recipient should receive the chat message");
}

#[tokio::test]
async fn muc_private_message_routes_from_sender_room_nick_to_target_session() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "pm-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        room_jid
            .clone()
            .with_resource_str("bob")
            .expect("target occupant jid"),
    )));
    message.type_ = XmppMessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id("muc-pm-1".to_string()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "private through the room".to_string(),
    );
    message.payloads.push(
        Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .append(
                Element::builder("item", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "mallory@example.com/web",
                    )
                    .build(),
            )
            .build(),
    );
    message.payloads.push(
        Element::builder("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "spoofed-occupant",
            )
            .build(),
    );
    message.payloads.push(
        Element::builder("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
            .attr(
                minidom::rxml::xml_ncname!("by").to_owned(),
                room_jid.to_string(),
            )
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "spoofed-room-stanza",
            )
            .build(),
    );
    // A full-JID `by` for this room (room@service/nick) must also be stripped —
    // it does not parse as a bare JID but resolves to the room.
    message.payloads.push(
        Element::builder("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
            .attr(
                minidom::rxml::xml_ncname!("by").to_owned(),
                format!("{room_jid}/alice"),
            )
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "spoofed-room-fulljid-stanza",
            )
            .build(),
    );
    // A malformed `by` must be stripped conservatively.
    message.payloads.push(
        Element::builder("stanza-id", waddle_xmpp_core::xep0359::NS_SID)
            .attr(minidom::rxml::xml_ncname!("by").to_owned(), "!!not-a-jid!!")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "spoofed-malformed-stanza",
            )
            .build(),
    );
    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(responses.is_empty(), "MUC PM routes asynchronously");

    let outbound = bob_rx
        .try_recv()
        .expect("target occupant receives routed MUC PM");
    let xml = stanza_to_xml(&outbound.stanza);
    let routed = Element::from_str(&xml).expect("routed MUC PM XML");
    assert_eq!(routed.name(), "message");
    assert_eq!(
        routed.attr("from"),
        Some(format!("{room_jid}/alice").as_str())
    );
    assert_eq!(routed.attr("to"), Some(bob_jid.to_string().as_str()));
    assert_eq!(routed.attr("type"), Some("chat"));
    assert!(
        xml.contains("private through the room"),
        "body is preserved: {xml}"
    );
    let muc_user = routed
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .expect("XEP-0045 §7.5 PM should carry the muc#user marker");
    assert!(
        muc_user
            .get_child("item", waddle_xmpp::muc::presence::NS_MUC_USER)
            .is_none(),
        "routed MUC PM must not preserve caller-supplied muc#user metadata: {xml}"
    );
    // XEP-0421 Business Rules (#1268): the caller-supplied occupant-id
    // is stripped and the server stamps its own stable value derived
    // from the SENDER's bare JID.
    let stamped_occupant_id = routed
        .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
        .expect("routed MUC PM must carry the server-stamped occupant-id");
    let expected_occupant_id = waddle_xmpp::xep::xep0421::generate_occupant_id(
        &alice_jid.to_bare(),
        &room_jid,
        &state.deps.occupant_id_secret,
    );
    assert_eq!(
        stamped_occupant_id.attr("id"),
        Some(expected_occupant_id.as_str()),
        "routed MUC PM occupant-id must be the server-derived sender id: {xml}"
    );
    assert_ne!(
        stamped_occupant_id.attr("id"),
        Some("spoofed-occupant"),
        "routed MUC PM must not preserve caller-supplied occupant-id: {xml}"
    );
    // The server strips ALL client-supplied stanza-ids (bare-JID,
    // full-JID, and unparseable `by` were all injected above) and then
    // stamps exactly ONE of its own (#1257): the recipient archive's
    // XEP-0359 stanza-id (`by` = bob's bare JID), so the live copy and
    // the MAM row share one id space.
    let stanza_ids: Vec<_> = routed
        .children()
        .filter(|child| child.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID))
        .collect();
    assert_eq!(
        stanza_ids.len(),
        1,
        "routed MUC PM carries exactly the server-stamped recipient stanza-id: {xml}"
    );
    assert_eq!(
        stanza_ids[0].attr("by"),
        Some(bob_jid.to_bare().to_string().as_str()),
        "the stamped stanza-id is attributed to the recipient archive: {xml}"
    );
    for spoofed in [
        "spoofed-room-stanza",
        "spoofed-room-fulljid-stanza",
        "spoofed-malformed-stanza",
    ] {
        assert_ne!(
            stanza_ids[0].attr("id"),
            Some(spoofed),
            "caller-supplied stanza-ids must not survive: {xml}"
        );
    }

    let alice_archive = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &alice_jid.to_bare(),
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query sender MAM archive");
    let bob_archive = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &bob_jid.to_bare(),
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query recipient MAM archive");
    let sender_row = alice_archive.messages.last().expect("sender archive row");
    assert_eq!(
        sender_row.from.to_string(),
        alice_jid.to_bare().to_string(),
        "sender archive must preserve the sender account endpoint"
    );
    assert_eq!(
        sender_row.to.to_string(),
        format!("{room_jid}/bob"),
        "sender archive must retain the recipient occupant peer"
    );
    let recipient_row = bob_archive.messages.last().expect("recipient archive row");
    assert_eq!(
        recipient_row.from.to_string(),
        format!("{room_jid}/alice"),
        "recipient archive must retain the sender occupant peer"
    );
    assert_eq!(
        recipient_row.to.to_string(),
        bob_jid.to_bare().to_string(),
        "recipient archive must preserve the delivered account endpoint"
    );
}

#[tokio::test]
async fn muc_private_message_to_own_nick_archives_one_delivered_tuple() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "pm-self-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        room_jid
            .clone()
            .with_resource_str("alice")
            .expect("self occupant jid"),
    )));
    message.type_ = XmppMessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id("muc-pm-self-1".to_string()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "private note to self".to_string(),
    );

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(responses.is_empty(), "self-PM routes asynchronously");

    let archive = state
        .deps
        .protocol
        .mam_storage
        .query_messages(
            &alice_jid.to_bare(),
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query self-PM archive");
    let self_rows: Vec<_> = archive
        .messages
        .iter()
        .filter(|row| row.body.as_deref() == Some("private note to self"))
        .collect();
    assert_eq!(self_rows.len(), 1, "self-PM must archive exactly once");
    assert_eq!(self_rows[0].from.to_string(), format!("{room_jid}/alice"));
    assert_eq!(self_rows[0].to.to_string(), alice_jid.to_bare().to_string());
}

#[tokio::test]
async fn muc_private_message_routes_normal_type_to_target_session() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "pm-normal-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        room_jid
            .clone()
            .with_resource_str("bob")
            .expect("target occupant jid"),
    )));
    message.type_ = XmppMessageType::Normal;
    message.id = Some(xmpp_parsers::message::Id("muc-pm-normal-1".to_string()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "normal private through the room".to_string(),
    );

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(responses.is_empty(), "normal MUC PM routes asynchronously");

    let outbound = bob_rx
        .try_recv()
        .expect("target occupant receives routed normal MUC PM");
    let xml = stanza_to_xml(&outbound.stanza);
    let routed = Element::from_str(&xml).expect("routed normal MUC PM XML");
    assert_eq!(
        routed.attr("from"),
        Some(format!("{room_jid}/alice").as_str())
    );
    assert_eq!(routed.attr("to"), Some(bob_jid.to_string().as_str()));
    assert!(
        routed.attr("type").is_none() || routed.attr("type") == Some("normal"),
        "normal message type should remain normal/omitted: {xml}"
    );
    assert!(
        xml.contains("normal private through the room"),
        "body is preserved: {xml}"
    );
}

#[tokio::test]
async fn muc_groupchat_to_occupant_jid_is_rejected_with_bad_request() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "pm-groupchat-room@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        room_jid
            .clone()
            .with_resource_str("bob")
            .expect("target occupant jid"),
    )));
    message.type_ = XmppMessageType::Groupchat;
    message.id = Some(xmpp_parsers::message::Id(
        "muc-pm-groupchat-bad-request".to_string(),
    ));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "must not be broadcast".to_string(),
    );

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert_eq!(
        responses.len(),
        1,
        "groupchat-to-occupant response: {responses:?}"
    );
    assert!(responses[0].contains("bad-request"));
    assert!(
        bob_rx.try_recv().is_err(),
        "groupchat addressed to room/nick must not broadcast to the room"
    );
}

#[tokio::test]
async fn muc_private_message_rejects_non_occupant_sender() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "pm-authz-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");
    let mallory_jid: FullJid = "mallory@example.com/web".parse().expect("mallory jid");

    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        room_jid
            .clone()
            .with_resource_str("bob")
            .expect("target occupant jid"),
    )));
    message.type_ = XmppMessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id("muc-pm-nonoccupant".to_string()));

    let responses = handle_message_for_test(state.as_ref(), &mallory_jid, None, message).await;
    assert_eq!(
        responses.len(),
        1,
        "non-occupant PM response: {responses:?}"
    );
    assert!(responses[0].contains("not-acceptable"));
    assert!(
        bob_rx.try_recv().is_err(),
        "non-occupant PM must not be routed to the target"
    );
}

#[tokio::test]
async fn muc_private_message_rejects_client_authored_inbox_payload_before_routing() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "pm-payload-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        room_jid
            .clone()
            .with_resource_str("bob")
            .expect("target occupant jid"),
    )));
    message.type_ = XmppMessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id(
        "muc-pm-waddle-payload".to_string(),
    ));
    message
        .payloads
        .push(Element::builder("entry", waddle_xmpp::xep::NS_WADDLE_INBOX).build());

    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert_eq!(
        responses.len(),
        1,
        "payload rejection response: {responses:?}"
    );
    assert!(responses[0].contains("bad-request"));
    assert!(
        bob_rx.try_recv().is_err(),
        "rejected PM must not be routed to the target"
    );
}

#[tokio::test]
async fn muc_mediated_decline_is_forwarded_from_room_to_inviter() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "decline-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let decliner_jid: FullJid = "hecate@example.com/broom".parse().expect("decliner jid");

    let (alice_tx, mut alice_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &alice_jid, alice_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    // #1264: a decline only forwards when the outstanding-invite ledger
    // says alice actually invited hecate to this room.
    crate::server::routes::websocket::muc_invites::record_invite(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::routes::websocket::muc_invites::OutstandingInvite {
            room: room_jid.clone(),
            invitee: decliner_jid.to_bare(),
            inviter: alice_jid.to_bare(),
        },
    )
    .await
    .expect("seed outstanding invite");

    let decline_payload = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(
            Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    alice_jid.to_bare().to_string(),
                )
                .append(
                    Element::builder("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
                        .append("too busy")
                        .build(),
                )
                .build(),
        )
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("decline-1".to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(decline_payload);
    let responses = handle_message_for_test(state.as_ref(), &decliner_jid, None, message).await;
    assert!(
        responses.is_empty(),
        "mediated decline routes asynchronously"
    );

    let outbound = alice_rx
        .try_recv()
        .expect("inviter receives mediated decline");
    let xml = stanza_to_xml(&outbound.stanza);
    let mediated = Element::from_str(&xml).expect("mediated decline XML");
    assert_eq!(mediated.name(), "message");
    assert_eq!(mediated.attr("from"), Some(room_jid.to_string().as_str()));
    assert_eq!(
        mediated.attr("to"),
        Some(alice_jid.to_bare().to_string().as_str()),
        "the decline is addressed to the ledger-recorded inviter"
    );
    let decline = mediated
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .and_then(|x| x.get_child("decline", waddle_xmpp::muc::presence::NS_MUC_USER))
        .expect("decline payload");
    assert_eq!(decline.attr("from"), Some("hecate@example.com"));
    assert_eq!(
        decline
            .get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
            .map(|reason| reason.text()),
        Some("too busy".to_string())
    );
    assert!(
        decline.attr("to").is_none(),
        "mediated decline rewrites to='…' into from='decliner': {xml}"
    );

    // The ledger row is consumed: a second decline is refused.
    let leftover = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room_jid,
        &decliner_jid.to_bare(),
    )
    .await
    .expect("ledger lookup");
    assert!(leftover.is_empty(), "delivered decline consumes the invite");
}

/// #1264: the ledger-recorded inviter is authoritative for decline
/// routing — a client-supplied `to` naming someone else must not make
/// the room deliver a decline to that third party.
#[tokio::test]
async fn muc_mediated_decline_routes_to_ledger_inviter_not_client_supplied_target() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "decline-full-room@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let mallory_jid: FullJid = "mallory@example.com/web".parse().expect("mallory jid");
    let decliner_jid: FullJid = "hecate@example.com/broom".parse().expect("decliner jid");

    let (alice_tx, mut alice_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &alice_jid, alice_tx).await;
    let (mallory_tx, mut mallory_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &mallory_jid, mallory_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    crate::server::routes::websocket::muc_invites::record_invite(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::routes::websocket::muc_invites::OutstandingInvite {
            room: room_jid.clone(),
            invitee: decliner_jid.to_bare(),
            inviter: alice_jid.to_bare(),
        },
    )
    .await
    .expect("seed outstanding invite");

    // The decliner tries to aim the decline at mallory.
    let decline_payload = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(
            Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    mallory_jid.to_bare().to_string(),
                )
                .build(),
        )
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("decline-full-1".to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(decline_payload);

    let responses = handle_message_for_test(state.as_ref(), &decliner_jid, None, message).await;
    assert!(
        responses.is_empty(),
        "mediated decline routes asynchronously"
    );

    assert!(
        mallory_rx.try_recv().is_err(),
        "the decline must not reach the client-supplied third party"
    );
    let outbound = alice_rx
        .try_recv()
        .expect("the actual inviter receives the mediated decline");
    let xml = stanza_to_xml(&outbound.stanza);
    let mediated = Element::from_str(&xml).expect("mediated decline XML");
    assert_eq!(mediated.attr("from"), Some(room_jid.to_string().as_str()));
    assert_eq!(
        mediated.attr("to"),
        Some(alice_jid.to_bare().to_string().as_str())
    );
    let decline = mediated
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .and_then(|x| x.get_child("decline", waddle_xmpp::muc::presence::NS_MUC_USER))
        .expect("decline payload");
    assert_eq!(decline.attr("from"), Some("hecate@example.com"));
}

/// #1264: without an outstanding invitation the decline is refused —
/// previously anyone could fabricate a "declined your invitation"
/// message toward any occupant.
#[tokio::test]
async fn muc_mediated_decline_without_outstanding_invite_is_refused() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "decline-spoof-room@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let decliner_jid: FullJid = "hecate@example.com/broom".parse().expect("decliner jid");

    let (alice_tx, mut alice_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &alice_jid, alice_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let decline_payload = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(
            Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    alice_jid.to_bare().to_string(),
                )
                .build(),
        )
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid)));
    message.id = Some(xmpp_parsers::message::Id("decline-spoof-1".to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(decline_payload);

    let responses = handle_message_for_test(state.as_ref(), &decliner_jid, None, message).await;
    assert_eq!(
        responses.len(),
        1,
        "fabricated decline must be refused: {responses:?}"
    );
    assert!(
        responses[0].contains("forbidden"),
        "fabricated decline must be <forbidden/>: {responses:?}"
    );
    assert!(
        alice_rx.try_recv().is_err(),
        "a fabricated decline must never reach the named occupant"
    );
}

/// #1264: a legitimate decline to an OFFLINE inviter is queued in the
/// pending-delivery store instead of silently dropped.
#[tokio::test]
async fn muc_mediated_decline_to_offline_inviter_is_queued_durably() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "decline-offline-room@muc.example.com"
        .parse()
        .expect("room jid");
    let inviter: BareJid = "alice@example.com".parse().expect("inviter jid");
    let decliner_jid: FullJid = "hecate@example.com/broom".parse().expect("decliner jid");

    crate::server::routes::websocket::muc_invites::record_invite(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::routes::websocket::muc_invites::OutstandingInvite {
            room: room_jid.clone(),
            invitee: decliner_jid.to_bare(),
            inviter: inviter.clone(),
        },
    )
    .await
    .expect("seed outstanding invite");

    let decline_payload = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(
            Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    inviter.to_string(),
                )
                .build(),
        )
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("decline-offline-1".to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(decline_payload);

    let responses = handle_message_for_test(state.as_ref(), &decliner_jid, None, message).await;
    assert!(
        responses.is_empty(),
        "queued decline is a silent success for the decliner: {responses:?}"
    );

    let pending = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&inviter)
        .await
        .expect("pending list");
    assert_eq!(
        pending.len(),
        1,
        "offline inviter must get a durable pending-delivery row"
    );

    let leftover = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room_jid,
        &decliner_jid.to_bare(),
    )
    .await
    .expect("ledger lookup");
    assert!(leftover.is_empty(), "queued decline consumes the invite");
}

#[tokio::test]
async fn handle_message_direct_chat_to_bare_jid_fans_out_to_all_connected_resources() {
    let state = create_test_websocket_state().await;
    let sender_jid: FullJid = "bob@example.com/phone".parse().expect("sender jid");
    let recipient_web: FullJid = "alice@example.com/web-123".parse().expect("recipient web");
    let recipient_mobile: FullJid = "alice@example.com/mobile-456"
        .parse()
        .expect("recipient mobile");

    let (web_tx, mut web_rx) = mpsc::channel(8);
    let (mobile_tx, mut mobile_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_web.clone(), web_tx);
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_mobile.clone(), mobile_tx);
    // RFC 6121 §8.5.2.1: bare-JID delivery selects available
    // resources by presence priority. The dispatcher path enforces
    // this; mark both recipient resources available so fan-out
    // reaches them.
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&recipient_web, true, 0);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&recipient_mobile, true, 0);

    // ADR-0017 Phase 1 Slice 1: bare-JID selection now sources its candidate
    // set + RFC ranking from the actor tree (intersected with DashMap
    // liveness), so mirroring these resources into the actor is REQUIRED for
    // this test to resolve live targets — not merely pre-warming. The
    // dual-registration mirror shares the same Arc-backed entries as the
    // DashMap registration, and `update_presence` above mutates the shared
    // atomics, so the actor observes the same availability.
    for jid in [&recipient_web, &recipient_mobile] {
        let entry = state
            .deps
            .protocol
            .connection_registry
            .get_entry(jid)
            .expect("entry registered above");
        let registered = crate::server::dual_registration::mirror_register(
            &state.deps.protocol.user_registry,
            jid.clone(),
            entry,
        )
        .await;
        assert!(registered, "test dual-registration should confirm {jid}");
    }

    let responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        None,
        parse_message_for_test(
            "<message xmlns='jabber:client' to='alice@example.com' type='chat' id='dm-bare-1'>\
                <body>hello all resources</body>\
             </message>",
        ),
    )
    .await;

    assert!(responses.is_empty(), "plain DM should not echo to sender");

    let mut web_chat = None;
    while let Ok(outbound) = web_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("hello all resources") && !xml.contains("urn:xmpp:carbons:2") {
            web_chat = Some(xml);
            break;
        }
    }

    let mut mobile_chat = None;
    while let Ok(outbound) = mobile_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("hello all resources") && !xml.contains("urn:xmpp:carbons:2") {
            mobile_chat = Some(xml);
            break;
        }
    }

    let _web_xml = web_chat.expect("web resource should receive original bare-JID message");
    let _mobile_xml =
        mobile_chat.expect("mobile resource should receive original bare-JID message");
    // RFC 6121 §8.5.2.1.1: bare-JID fan-out delivers the original
    // stanza to each available resource without rewriting the
    // `to` attribute. The dispatcher path preserves this; the prior
    // `handle_message` behavior rewrote `to` to the per-resource full JID,
    // which was a deviation from the RFC. Both resources received
    // the stanza — the reachability semantic — and that is what
    // this unit test now asserts.
}

#[tokio::test]
async fn handle_message_direct_chat_sends_sent_carbon_to_opted_in_sibling_resource() {
    let state = create_test_websocket_state().await;
    // #1246: an unregistered recipient would bounce with
    // <service-unavailable/>; this test is about the sender-side
    // carbon, so give the offline recipient a real account.
    crate::server::routes::websocket::tests::seed_local_account(state.as_ref(), "ghost").await;
    let sender_jid: FullJid = "alice@example.com/phone".parse().expect("sender jid");
    let sibling_jid: FullJid = "alice@example.com/desktop".parse().expect("sibling jid");

    let (sender_tx, _sender_rx) = mpsc::channel(8);
    let (sibling_tx, mut sibling_rx) = mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(sender_jid.clone(), sender_tx);
    state
        .deps
        .protocol
        .connection_registry
        .register_with_carbons(sibling_jid.clone(), sibling_tx, true);

    let responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        None,
        parse_message_for_test(
            "<message xmlns='jabber:client' to='ghost@example.com' type='chat' id='sent-carbon-1'>\
                <body>sent carbon over websocket</body>\
             </message>",
        ),
    )
    .await;

    assert!(responses.is_empty(), "plain DM should not echo to sender");

    let mut sent_carbon = None;
    while let Ok(outbound) = sibling_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("urn:xmpp:carbons:2") && xml.contains("<sent") {
            sent_carbon = Some(xml);
            break;
        }
    }

    let carbon_xml = sent_carbon.expect("opted-in sibling should receive sent carbon");
    assert!(
        carbon_xml.contains("sent carbon over websocket"),
        "sent carbon should preserve message body: {carbon_xml}"
    );
}

#[tokio::test]
async fn handle_message_direct_chat_sends_received_carbon_to_opted_in_sibling_resource() {
    let state = create_test_websocket_state().await;
    let sender_jid: FullJid = "bob@example.com/phone".parse().expect("sender jid");
    let recipient_jid: FullJid = "alice@example.com/phone".parse().expect("recipient jid");
    let sibling_jid: FullJid = "alice@example.com/desktop".parse().expect("sibling jid");

    let (recipient_tx, mut recipient_rx) = mpsc::channel(8);
    let (sibling_tx, mut sibling_rx) = mpsc::channel(8);
    // ADR-0017 Slice 2: delivery reads the actor tree, so register into both
    // tiers. The sibling has carbons enabled — set it on the shared entry after
    // registration (the Arc is shared with the actor).
    register_test_connection(state.as_ref(), &recipient_jid, recipient_tx).await;
    register_test_connection(state.as_ref(), &sibling_jid, sibling_tx).await;
    state
        .deps
        .protocol
        .connection_registry
        .get_entry(&sibling_jid)
        .expect("sibling entry")
        .carbons_enabled
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let frame = format!(
        "<message xmlns='jabber:client' to='{recipient_jid}' type='chat' id='recv-carbon-1'>\
                <body>received carbon over websocket</body>\
             </message>"
    );

    // Build alice/phone's per-connection state machine so we can
    // drive the recipient pass the dispatcher path now owns. In
    // production this happens automatically via alice/phone's
    // main loop dispatching the queued
    // `DeliveryKind::PeerStanza`; the unit test reproduces the
    // same step explicitly so we observe the recipient-side
    // received-carbon fan-out.
    let mut recipient_conn = WsConnState::new();
    recipient_conn.phase = ConnectionPhase::ready(recipient_jid.clone(), false);
    recipient_conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        recipient_jid.clone(),
        false,
        Blocklist::empty(),
    );

    let responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        None,
        parse_message_for_test(frame.as_str()),
    )
    .await;

    assert!(responses.is_empty(), "plain DM should not echo to sender");

    // Pump alice/phone's queued PeerStanza through her SM so the
    // recipient pass runs and emits SendCarbons (received) to
    // alice/desktop.
    while let Ok(outbound) = recipient_rx.try_recv() {
        if !matches!(outbound.kind, DeliveryKind::PeerStanza) {
            continue;
        }
        let sm = recipient_conn.state_machine.as_mut().expect("alice SM");
        let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
        let deps = build_interpret_deps(state.as_ref(), None);
        let _ = drive_interpret_loop(events, sm, &deps).await;
    }

    let mut received_carbon = None;
    while let Ok(outbound) = sibling_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        if xml.contains("urn:xmpp:carbons:2") && xml.contains("<received") {
            received_carbon = Some(xml);
            break;
        }
    }

    let carbon_xml = received_carbon.expect("opted-in sibling should receive received carbon");
    assert!(
        carbon_xml.contains("received carbon over websocket"),
        "received carbon should preserve message body: {carbon_xml}"
    );
}

#[tokio::test]
async fn direct_messages_round_trip_through_inbox_query_and_mark_read() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{}' type='chat' id='dm-inbox-1'>\
                <body>Hello from Alice</body>\
             </message>",
        bob_jid.to_bare()
    );
    let responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    assert!(responses.is_empty(), "plain DM should not echo to sender");

    // XEP-0430: streamed `<message><entry/></message>` per row plus a
    // closing `<iq><fin/></iq>`. The handler returns each row's
    // message stanza followed by the fin IQ as the last element.
    let inbox_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-1'>\
                <inbox xmlns='urn:xmpp:inbox:1'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let inbox_responses = handle_iq(
        inbox_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &ready_phase(&bob_jid),
    )
    .await;
    let entry_xml = inbox_responses
        .iter()
        .find(|frame| frame.contains("<entry "))
        .expect("at least one streamed inbox entry");
    assert!(
        entry_xml.contains("jid='alice@example.com'"),
        "streamed inbox entry should reference Alice: {entry_xml}"
    );
    assert!(
        entry_xml.contains("unread='1'"),
        "streamed inbox entry should report one unread DM: {entry_xml}"
    );
    let fin_xml = inbox_responses
        .last()
        .expect("inbox response ends with fin IQ");
    assert!(
        fin_xml.contains("<fin"),
        "last frame is the XEP-0430 fin IQ: {fin_xml}"
    );
    assert!(
        fin_xml.contains("total='1'"),
        "fin reports a single matched conversation: {fin_xml}"
    );

    let mark_read = format!(
        "<iq xmlns='jabber:client' type='set' to='{}' id='inbox-2'>\
                <mark-read xmlns='urn:waddle:inbox:0' partner='alice@example.com'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let mark_read_responses = handle_iq(
        mark_read.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &ready_phase(&bob_jid),
    )
    .await;
    let mark_read_xml = mark_read_responses.first().expect("mark-read result");
    assert!(
        mark_read_xml.contains("type='result'"),
        "mark-read should succeed: {mark_read_xml}"
    );

    let unread_only_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-3'>\
                <inbox xmlns='urn:xmpp:inbox:1' unread-only='true'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let unread_only_responses = handle_iq(
        unread_only_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready_phase(&bob_jid),
    )
    .await;
    let unread_only_fin = unread_only_responses
        .last()
        .expect("unread-only response ends with fin IQ");
    assert!(
        unread_only_fin.contains("total='0'"),
        "mark-read clears unread, fin should report no matches: {unread_only_fin}"
    );
    assert!(
        unread_only_responses
            .iter()
            .all(|frame| !frame.contains("<entry ")),
        "unread-only query should emit no entry messages after mark-read"
    );
}

#[tokio::test]
async fn inbox_query_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let pending_jid: FullJid = "bob@example.com/pending".parse().expect("pending jid");
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let mut blocklist_interested = false;
    let frame = r#"<iq xmlns='jabber:client' type='get' to='bob@example.com' id='inbox-prebind-1'><inbox xmlns='urn:xmpp:inbox:1'/></iq>"#;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        blocklist_interested: &mut blocklist_interested,
        registry_owner: None,
        state_machine: None,
        ordered_relay_origin: None,
    };
    let responses = handle_iq_with_conn_state(
        parse_iq_for_test(frame),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ConnectionPhase::authenticated(&pending_jid),
        &mut conn_state,
    )
    .await
    .frames;

    let response = responses.first().expect("inbox error response");
    assert!(
        response.contains("not-authorized"),
        "pre-bind inbox IQ should be rejected: {response}"
    );
}

#[tokio::test]
async fn encrypted_sfs_messages_without_bodies_still_project_into_inbox() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{}' type='chat' id='dm-esfs-1'>\
                <file-sharing xmlns='urn:xmpp:sfs:0'>\
                    <sources xmlns='urn:xmpp:sfs:0'>\
                        <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                            <key>a2V5</key>\
                            <iv>aXY=</iv>\
                            <sources xmlns='urn:xmpp:sfs:0'>\
                                <url-data target='https://files.example.com/secret.enc'/>\
                            </sources>\
                        </encrypted>\
                    </sources>\
                </file-sharing>\
             </message>",
        bob_jid.to_bare()
    );
    let responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    assert!(
        responses.is_empty(),
        "encrypted file-sharing DM should not echo to sender"
    );

    let inbox_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-esfs-1'>\
                <inbox xmlns='urn:xmpp:inbox:1'/>\
             </iq>",
        bob_jid.to_bare()
    );
    let inbox_responses = handle_iq(
        inbox_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready_phase(&bob_jid),
    )
    .await;
    let entry_xml = inbox_responses
        .iter()
        .find(|frame| frame.contains("<entry "))
        .expect("streamed inbox entry for the bodyless file-sharing DM");
    assert!(
        entry_xml.contains("jid='alice@example.com'"),
        "encrypted file-sharing inbox entry should target Alice: {entry_xml}"
    );
    assert!(
        entry_xml.contains("unread='1'"),
        "encrypted file-sharing inbox entry should increment unread: {entry_xml}"
    );
    assert!(
        !entry_xml.contains("preview="),
        "bodyless encrypted file-sharing message should not invent preview text: {entry_xml}"
    );
}

#[tokio::test]
async fn groupchat_messages_are_archived_and_returned_via_mam() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let waddle_id = "waddle-alpha";
    let channel_id = "channel-bravo";
    let room_jid: BareJid = format!("{channel_id}@muc.example.com")
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("sender jid");
    // #229 PR18 cutover: groupchat reflections flow through the
    // connection registry as PeerStanza, so register the sender's
    // outbound channel before driving `handle_message`.
    let (sender_tx, mut sender_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &sender_jid, sender_tx).await;
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        waddle_id.to_string(),
        channel_id.to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join room");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{room_jid}' type='groupchat' id='client-msg-1'>\
                <body>Hello from WebSocket</body>\
             </message>"
    );
    let _message_responses = handle_message_for_test(
        state.as_ref(),
        &sender_jid,
        Some(&session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    let echo_stanza = sender_rx
        .try_recv()
        .expect("sender echo queued on outbound channel");
    let _echo_xml = stanza_to_xml(&echo_stanza.stanza);

    let mam_query = format!(
        "<iq xmlns='jabber:client' type='set' to='{room_jid}' id='mam-1'>\
                <query xmlns='urn:xmpp:mam:2' queryid='q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                    <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>\
                </query>\
             </iq>"
    );
    let mam_responses = handle_iq(
        mam_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&sender_jid),
    )
    .await;

    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("urn:xmpp:mam:2") && stanza.contains("<result")),
        "expected at least one MAM result stanza, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("Hello from WebSocket")),
        "expected archived body in MAM replay, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("<fin") && stanza.contains("urn:xmpp:mam:2")),
        "expected MAM fin stanza, got: {:?}",
        mam_responses
    );
}

#[tokio::test]
async fn groupchat_preview_request_is_stamped_and_archived_without_private_payload() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "preview-pipeline@muc.example.com"
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("sender jid");
    let (sender_tx, mut sender_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &sender_jid, sender_tx).await;
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        "waddle-alpha".to_string(),
        "preview-pipeline".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: current_admission_revision(&room_actor).await,
        })
        .await
        .expect("join room");

    let preview = waddle_xmpp::xep::LinkPreviewTokenData {
        sender_jid: "alice@example.com".parse().expect("sender bare jid"),
        scope_jid: room_jid.clone(),
        original_url: url::Url::parse("https://the.link.example/what-was-linked").expect("url"),
        normalized_url: url::Url::parse("https://the.link.example/what-was-linked").expect("url"),
        title: Some("The Best Webpage".to_string()),
        description: Some("Plain text preview".to_string()),
        image: None,
        video: None,
        native_video: None,
        player: None,
        expires_at_unix: chrono::Utc::now().timestamp() + 300,
    };
    let token =
        waddle_xmpp::xep::encode_link_preview_token(&preview, state.deps.occupant_id_secret.key());
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(
        "preview-pipeline-msg".to_string(),
    ));
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "read https://the.link.example/what-was-linked".to_string(),
    );
    message
        .payloads
        .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

    let responses =
        handle_message_for_test(state.as_ref(), &sender_jid, Some(&session), message).await;
    assert!(
        responses.is_empty(),
        "valid preview request should not produce direct errors: {responses:?}"
    );
    let echo_stanza = sender_rx
        .try_recv()
        .expect("sender echo queued on outbound channel");
    let echo_xml = stanza_to_xml(&echo_stanza.stanza);
    assert!(
        echo_xml.contains(waddle_xmpp::xep::NS_RDF_SYNTAX) && echo_xml.contains("The Best Webpage"),
        "echo should include stamped XEP-0511 metadata: {echo_xml}"
    );
    assert!(
        !echo_xml.contains(waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW),
        "private preview request must be stripped from echo: {echo_xml}"
    );

    let form = Element::builder("x", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append("urn:xmpp:mam:2")
                        .build(),
                )
                .build(),
        )
        .build();
    let rsm = Element::builder("set", "http://jabber.org/protocol/rsm")
        .append(
            Element::builder("max", "http://jabber.org/protocol/rsm")
                .append("50")
                .build(),
        )
        .build();
    let query = Element::builder("query", "urn:xmpp:mam:2")
        .attr(
            minidom::rxml::xml_ncname!("queryid").to_owned(),
            "q-preview",
        )
        .append(form)
        .append(rsm)
        .build();
    let mam_query = iq_set_frame("mam-preview", room_jid.as_str(), query);
    let mam_responses = handle_iq(
        mam_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&sender_jid),
    )
    .await;

    let archived_preview = mam_responses
        .iter()
        .find(|stanza| stanza.contains("The Best Webpage"))
        .unwrap_or_else(|| panic!("expected archived preview in MAM result: {mam_responses:?}"));
    assert!(
        archived_preview.contains(waddle_xmpp::xep::NS_RDF_SYNTAX),
        "MAM result should include stamped XEP-0511 metadata: {archived_preview}"
    );
    assert!(
        !archived_preview.contains(waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW),
        "private preview request must not be archived: {archived_preview}"
    );
}

#[tokio::test]
async fn personal_mam_query_uses_ready_phase_when_sidecar_session_is_missing() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/mobile", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let message_xml = format!(
        "<message xmlns='jabber:client' to='{}' type='chat' id='dm-mam-1'>\
                <body>Hello from Alice</body>\
             </message>",
        bob_jid.to_bare()
    );
    let message_responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        parse_message_for_test(message_xml.as_str()),
    )
    .await;
    assert!(
        message_responses.is_empty(),
        "plain DM should not echo to sender"
    );

    let mam_query = format!(
        "<iq xmlns='jabber:client' type='set' to='{}' id='mam-personal-1'>\
                <query xmlns='urn:xmpp:mam:2' queryid='q1'>\
                    <x xmlns='jabber:x:data' type='submit'>\
                        <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                    </x>\
                    <set xmlns='http://jabber.org/protocol/rsm'><max>50</max></set>\
                </query>\
             </iq>",
        bob_jid.to_bare()
    );
    let mam_responses = handle_iq(
        mam_query.as_str(),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;

    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("urn:xmpp:mam:2") && stanza.contains("<result")),
        "expected personal MAM result stanza for resumed ready phase, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains("Hello from Alice")),
        "expected archived DM in personal MAM replay, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .any(|stanza| stanza.contains(&format!("to='{}'", bob_jid))
                || stanza.contains(&format!("to='{}'", bob_jid))),
        "expected MAM results addressed to the resumed bound resource, got: {:?}",
        mam_responses
    );
    assert!(
        mam_responses
            .iter()
            .all(|stanza| !stanza.contains("unknown@localhost")),
        "resumed personal MAM must not fall back to unknown recipient: {:?}",
        mam_responses
    );
}

#[test]
fn stanza_to_xml_includes_payloads() {
    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().unwrap(),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com".parse::<jid::BareJid>().unwrap(),
    ));
    msg.id = Some(xmpp_parsers::message::Id("test-1".into()));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "Hello".into());

    let embed = xmpp_parsers::minidom::Element::builder("repo", "urn:waddle:test-extension:1")
        .attr(minidom::rxml::xml_ncname!("owner").to_owned(), "cuenv")
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "cuenv")
        .build();
    msg.payloads.push(embed);

    let xml = stanza_to_xml(&Stanza::Message(msg));

    assert!(xml.contains("<body>Hello</body>"), "body must be present");
    assert!(
        xml.contains("urn:waddle:test-extension:1"),
        "payload namespace must be serialized: {xml}"
    );
    assert!(
        xml.contains("cuenv"),
        "payload attributes must be serialized: {xml}"
    );
    // Must not contain XML declaration inside the message
    assert!(
        !xml.contains("<?xml"),
        "payload must not include XML declaration: {xml}"
    );
    // The whole thing must be a single <message>...</message>
    assert!(
        xml.starts_with("<message"),
        "must start with <message: {xml}"
    );
    assert!(
        xml.ends_with("</message>"),
        "must end with </message>: {xml}"
    );
}

#[test]
fn stanza_to_xml_no_payloads_still_works() {
    let mut msg = xmpp_parsers::message::Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().unwrap(),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com".parse::<jid::BareJid>().unwrap(),
    ));
    msg.id = Some(xmpp_parsers::message::Id("test-2".into()));
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "No embeds".into());

    let xml = stanza_to_xml(&Stanza::Message(msg));
    assert!(xml.contains("<body>No embeds</body>"));
    assert!(xml.ends_with("</message>"));
}

#[test]
fn parse_message_stanza_preserves_thread_and_reply() {
    let xml = r#"<message to="room@muc.localhost" type="groupchat" id="msg-1">
            <body>Hello</body>
            <thread>thread-root</thread>
            <reply xmlns="urn:xmpp:reply:0" id="parent-msg" to="alice@localhost"/>
        </message>"#;

    let parsed = parse_message_for_test(xml);
    assert_eq!(parsed.id.as_ref().map(|id| id.0.as_str()), Some("msg-1"));
    assert_eq!(
        parsed.thread.as_ref().map(|t| t.id.as_str()),
        Some("thread-root")
    );
    assert!(parsed
        .payloads
        .iter()
        .any(|p| p.name() == "reply" && p.ns() == "urn:xmpp:reply:0"));
}

#[test]
fn stanza_to_xml_preserves_thread_and_reply() {
    let xml = r#"<message to="room@muc.localhost" type="groupchat" id="msg-2">
            <body>Follow-up</body>
            <thread>thread-root</thread>
            <reply xmlns="urn:xmpp:reply:0" id="msg-1" to="alice@localhost"/>
        </message>"#;

    let parsed = parse_message_for_test(xml);
    let rendered = stanza_to_xml(&Stanza::Message(parsed));
    let reparsed = parse_message_for_test(&rendered);
    assert_eq!(
        reparsed.thread.as_ref().map(|thread| thread.id.as_str()),
        Some("thread-root"),
        "rendered stanza: {rendered}"
    );
    assert!(reparsed.payloads.iter().any(|payload| {
        payload.name() == "reply"
            && payload.ns() == "urn:xmpp:reply:0"
            && payload.attr("id") == Some("msg-1")
            && payload.attr("to") == Some("alice@localhost")
    }));
}

#[test]
fn xep_0201_thread_reattach_ignores_unrelated_namespaced_thread_payload() {
    // RFC 6121 §5.2.5 scopes `<thread/>` to the enclosing message's
    // namespace. If a stanza already contains a `<thread>` payload
    // in some other namespace (an unrelated extension), the
    // reattach branch must NOT see it as a conflict and skip
    // serializing the typed `message.thread` field — that would
    // drop the actual conversation thread on the wire (Copilot
    // review on PR #305).
    use xmpp_parsers::message::{Message, MessageType, Thread};
    use xmpp_parsers::minidom::Element;

    let mut msg = Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().expect("jid"),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com/web"
            .parse::<jid::FullJid>()
            .expect("jid"),
    ));
    msg.id = Some(xmpp_parsers::message::Id("msg-ns".to_string()));
    msg.type_ = MessageType::Chat;
    msg.bodies
        .insert(xmpp_parsers::message::Lang(String::new()), "hi".to_string());
    msg.thread = Some(Thread {
        id: "conversation-thread".to_string(),
        parent: None,
    });
    // Unrelated extension element happening to be named "thread"
    // in a different namespace — must not suppress reattachment.
    msg.payloads.push(
        Element::builder("thread", "urn:example:other:0")
            .attr(minidom::rxml::xml_ncname!("kind").to_owned(), "unrelated")
            .build(),
    );

    let rendered = stanza_to_xml(&Stanza::Message(msg));
    let reparsed = parse_message_for_test(&rendered);
    assert_eq!(
        reparsed.thread.as_ref().map(|t| t.id.as_str()),
        Some("conversation-thread"),
        "RFC 6121 thread must survive serialization despite unrelated <thread> in another ns; rendered: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// XEP-0045 §7.8 mediated invitations for regular (non-group-DM) rooms
// (#1248): previously these stanzas were silently dropped.
// ---------------------------------------------------------------------------

fn mediated_invite_message(
    room_jid: &BareJid,
    invitee: &BareJid,
    id: &str,
    reason: Option<&str>,
) -> xmpp_parsers::message::Message {
    let mut invite = Element::builder("invite", waddle_xmpp::muc::presence::NS_MUC_USER).attr(
        minidom::rxml::xml_ncname!("to").to_owned(),
        invitee.to_string(),
    );
    if let Some(reason) = reason {
        invite = invite.append(
            Element::builder("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
                .append(reason)
                .build(),
        );
    }
    let x = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(invite.build())
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id(id.to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(x);
    message
}

/// §7.8.2: the room adds `from` (the inviter) to the `<invite/>` and
/// relays it to the invitee from the room's own bare JID; the
/// outstanding-invite ledger records the relay (#1264).
#[tokio::test]
async fn xep0045_mediated_invite_relayed_from_room_to_invitee() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "invite-room@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let invitee: BareJid = "hecate@example.com".parse().expect("invitee jid");
    let invitee_jid: FullJid = "hecate@example.com/broom"
        .parse()
        .expect("invitee full jid");
    register_test_native_user(state.as_ref(), "hecate", "cauldron-pass").await;

    let (hecate_tx, mut hecate_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &invitee_jid, hecate_tx).await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let message = mediated_invite_message(&room_jid, &invitee, "invite-1", Some("dark rites"));
    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "mediated invite relays asynchronously: {responses:?}"
    );

    let outbound = hecate_rx
        .try_recv()
        .expect("invitee receives the mediated invitation");
    let xml = stanza_to_xml(&outbound.stanza);
    let mediated = Element::from_str(&xml).expect("mediated invite XML");
    assert_eq!(mediated.name(), "message");
    assert_eq!(
        mediated.attr("from"),
        Some(room_jid.to_string().as_str()),
        "§7.8.2: the invitation comes from the room itself"
    );
    assert_eq!(mediated.attr("to"), Some(invitee.to_string().as_str()));
    let invite = mediated
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .and_then(|x| x.get_child("invite", waddle_xmpp::muc::presence::NS_MUC_USER))
        .expect("invite payload");
    assert_eq!(
        invite.attr("from"),
        Some(alice_jid.to_bare().to_string().as_str()),
        "§7.8.2: the room MUST stamp the inviter into invite@from"
    );
    assert_eq!(
        invite
            .get_child("reason", waddle_xmpp::muc::presence::NS_MUC_USER)
            .map(|reason| reason.text()),
        Some("dark rites".to_string())
    );

    let ledger = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room_jid,
        &invitee,
    )
    .await
    .expect("ledger lookup");
    assert_eq!(
        ledger.len(),
        1,
        "relayed invite recorded in the ledger exactly once"
    );
    assert_eq!(ledger[0].inviter, alice_jid.to_bare());
}

/// §7.8: mediated invitations are an occupant action — a non-occupant
/// sender is refused.
#[tokio::test]
async fn xep0045_mediated_invite_from_non_occupant_rejected() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "invite-outsider@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let outsider_session = create_test_session(state.as_ref(), "mallory").await;
    let outsider: FullJid = "mallory@example.com/web".parse().expect("outsider jid");
    register_test_native_user(state.as_ref(), "hecate", "cauldron-pass").await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let invitee: BareJid = "hecate@example.com".parse().expect("invitee jid");
    let message = mediated_invite_message(&room_jid, &invitee, "invite-2", None);
    let responses =
        handle_message_for_test(state.as_ref(), &outsider, Some(&outsider_session), message).await;
    assert_eq!(responses.len(), 1, "non-occupant invite: {responses:?}");
    assert!(
        responses[0].contains("not-acceptable"),
        "non-occupant invite must be refused: {responses:?}"
    );
}

/// §7.8.2: "If the inviter supplies a non-existent JID, the room
/// SHOULD return an <item-not-found/> error to the inviter."
#[tokio::test]
async fn xep0045_mediated_invite_nonexistent_invitee_item_not_found() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "invite-ghost@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let ghost: BareJid = "ghost@example.com".parse().expect("ghost jid");
    let message = mediated_invite_message(&room_jid, &ghost, "invite-3", None);
    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert_eq!(responses.len(), 1, "ghost invite: {responses:?}");
    assert!(
        responses[0].contains("item-not-found"),
        "nonexistent invitee must be <item-not-found/>: {responses:?}"
    );
}

/// §7.8.2 note: members-only rooms restrict invitations to admins;
/// a plain member gets <forbidden/>, while an admin/owner invite
/// auto-adds the invitee to the member list.
#[tokio::test]
async fn xep0045_mediated_invite_members_only_restricted_to_admins_with_auto_add() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "invite-members-only@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let invitee: BareJid = "hecate@example.com".parse().expect("invitee jid");
    register_test_native_user(state.as_ref(), "hecate", "cauldron-pass").await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let room_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room exists");
    room_actor
        .ask(ChangeAffiliation {
            jid: alice_jid.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("grant owner");
    room_actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("grant member");
    let mut config = room_actor
        .ask(waddle_xmpp::muc::room_actor::GetConfig)
        .await
        .expect("config");
    config.members_only = true;
    room_actor
        .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
            config,
            effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("members-only config");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    // A plain member's invite is forbidden.
    let member_invite = mediated_invite_message(&room_jid, &invitee, "invite-4a", None);
    let responses =
        handle_message_for_test(state.as_ref(), &bob_jid, Some(&bob_session), member_invite).await;
    assert_eq!(responses.len(), 1, "member invite: {responses:?}");
    assert!(
        responses[0].contains("forbidden"),
        "member invite in a members-only room must be <forbidden/>: {responses:?}"
    );

    // The owner's invite succeeds and auto-adds the invitee.
    let owner_invite = mediated_invite_message(&room_jid, &invitee, "invite-4b", None);
    let responses = handle_message_for_test(
        state.as_ref(),
        &alice_jid,
        Some(&alice_session),
        owner_invite,
    )
    .await;
    assert!(
        responses.is_empty(),
        "owner invite relays asynchronously: {responses:?}"
    );
    let snapshot = room_actor
        .ask(waddle_xmpp::muc::room_actor::GetSnapshot)
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.room.get_affiliation(&invitee),
        Affiliation::Member,
        "§7.8.2: members-only invite must add the invitee to the member list"
    );
}

/// #1248: an OFFLINE invitee gets a durable pending-delivery row
/// instead of a silent drop.
#[tokio::test]
async fn xep0045_mediated_invite_offline_invitee_queued_durably() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "invite-offline@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let invitee: BareJid = "hecate@example.com".parse().expect("invitee jid");
    register_test_native_user(state.as_ref(), "hecate", "cauldron-pass").await;

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let message = mediated_invite_message(&room_jid, &invitee, "invite-5", None);
    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), message).await;
    assert!(
        responses.is_empty(),
        "offline invite queues durably: {responses:?}"
    );

    let pending = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&invitee)
        .await
        .expect("pending list");
    assert_eq!(
        pending.len(),
        1,
        "offline invitee must get a durable pending-delivery row"
    );

    // Anti-spam dedup (#1264 hardening): re-sending the identical
    // invite while one is outstanding is a silent success and MUST NOT
    // insert another pending-delivery row — repeated invites cannot
    // exhaust the invitee's offline quota.
    let repeat = mediated_invite_message(&room_jid, &invitee, "invite-5b", None);
    let responses =
        handle_message_for_test(state.as_ref(), &alice_jid, Some(&alice_session), repeat).await;
    assert!(responses.is_empty(), "duplicate invite: {responses:?}");
    let pending = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&invitee)
        .await
        .expect("pending list after duplicate");
    assert_eq!(
        pending.len(),
        1,
        "a duplicate invite must not add a second pending-delivery row"
    );
}

/// #1264: with invitations from SEVERAL inviters outstanding, the
/// decline's `to` selects which one is declined — and only that
/// inviter's row is consumed.
#[tokio::test]
async fn muc_mediated_decline_selects_inviter_by_to_among_multiple() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "decline-multi-room@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: BareJid = "alice@example.com".parse().expect("alice");
    let bob: BareJid = "bob@example.com".parse().expect("bob");
    let decliner_jid: FullJid = "hecate@example.com/broom".parse().expect("decliner jid");
    let alice_full: FullJid = "alice@example.com/web".parse().expect("alice full");

    let (alice_tx, mut alice_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &alice_full, alice_tx).await;

    for inviter in [&alice, &bob] {
        crate::server::routes::websocket::muc_invites::record_invite(
            state.deps.app_state.db_pool.global_actor().clone(),
            &crate::server::routes::websocket::muc_invites::OutstandingInvite {
                room: room_jid.clone(),
                invitee: decliner_jid.to_bare(),
                inviter: inviter.clone(),
            },
        )
        .await
        .expect("seed invite");
    }

    let decline_payload = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(
            Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    alice.to_string(),
                )
                .build(),
        )
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("decline-multi-1".to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(decline_payload);
    let responses = handle_message_for_test(state.as_ref(), &decliner_jid, None, message).await;
    assert!(responses.is_empty(), "decline routes: {responses:?}");

    let outbound = alice_rx.try_recv().expect("alice receives her decline");
    let xml = stanza_to_xml(&outbound.stanza);
    assert!(xml.contains("decline"), "decline payload delivered: {xml}");

    let remaining = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room_jid,
        &decliner_jid.to_bare(),
    )
    .await
    .expect("ledger lookup");
    assert_eq!(
        remaining.len(),
        1,
        "only the declined inviter's row is consumed"
    );
    assert_eq!(remaining[0].inviter, bob, "bob's invitation stays live");
}

/// #1264: with several invitations outstanding, a decline that names
/// none of the inviters is ambiguous — <bad-request/>, nothing
/// forwarded, nothing consumed.
#[tokio::test]
async fn muc_mediated_decline_ambiguous_target_is_bad_request() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "decline-ambiguous-room@muc.example.com"
        .parse()
        .expect("room jid");
    let decliner_jid: FullJid = "hecate@example.com/broom".parse().expect("decliner jid");

    for inviter in ["alice@example.com", "bob@example.com"] {
        crate::server::routes::websocket::muc_invites::record_invite(
            state.deps.app_state.db_pool.global_actor().clone(),
            &crate::server::routes::websocket::muc_invites::OutstandingInvite {
                room: room_jid.clone(),
                invitee: decliner_jid.to_bare(),
                inviter: inviter.parse().expect("inviter"),
            },
        )
        .await
        .expect("seed invite");
    }

    let decline_payload = Element::builder("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .append(
            Element::builder("decline", waddle_xmpp::muc::presence::NS_MUC_USER)
                .attr(
                    minidom::rxml::xml_ncname!("to").to_owned(),
                    "mallory@example.com",
                )
                .build(),
        )
        .build();
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some(xmpp_parsers::message::Id("decline-ambiguous-1".to_string()));
    message.type_ = XmppMessageType::Normal;
    message.payloads.push(decline_payload);
    let responses = handle_message_for_test(state.as_ref(), &decliner_jid, None, message).await;
    assert_eq!(responses.len(), 1, "ambiguous decline: {responses:?}");
    assert!(
        responses[0].contains("bad-request"),
        "ambiguous decline must be <bad-request/>: {responses:?}"
    );

    let remaining = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room_jid,
        &decliner_jid.to_bare(),
    )
    .await
    .expect("ledger lookup");
    assert_eq!(remaining.len(), 2, "nothing consumed on ambiguity");
}
