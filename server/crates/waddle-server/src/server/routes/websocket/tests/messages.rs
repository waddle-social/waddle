use super::*;

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
        .map(|body| body.0.clone());
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
                    .attr("type", "submit")
                    .append(
                        Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                            .attr("var", "FORM_TYPE")
                            .append(
                                Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                                    .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                                    .build(),
                            )
                            .build(),
                    )
                    .append(
                        Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                            .attr("var", "secret")
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
    message.id = Some("offline-push-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("push me".to_string()),
    );
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
    message.id = Some("offline-push-registration-late".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("candidate first".to_string()),
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
    message.id = Some("offline-push-no-mam".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("do not push without committed archive".to_string()),
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
    message.id = Some(message_id.to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("hello bob".to_string()),
    );
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
    message.id = Some(message_id.to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("hello bob".to_string()),
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
    .expect("create room");
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
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("groupchat-personal-mention-push".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("charlie, take a look".to_string()),
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
    .expect("create room");
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
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");

    let occupant_id = waddle_xmpp::xep::generate_occupant_id(
        &recipient,
        &room_jid,
        &state.deps.occupant_id_secret,
    );
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("groupchat-occupant-id-mention-push".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("@charlie, take a look".to_string()),
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
        .expect("create room");
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
                effective_affiliation: Affiliation::Member,
                local_domain: "example.com".to_string(),
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
        message.id = Some(message_id.to_string());
        message.type_ = XmppMessageType::Groupchat;
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("never means never".to_string()),
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
    .expect("create recovery room");
    let archive_stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
        "groupchat-recovery-archive",
        jid::Jid::from(room_jid.clone()),
    );
    let sender_jid: jid::Jid = "groupchat-recovery@muc.example.com/alice"
        .parse()
        .expect("sender jid");
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.from = Some(sender_jid.clone());
    message.id = Some("groupchat-recovery-wire".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("charlie, this should recover".to_string()),
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
        .expect("create room");
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
                effective_affiliation: Affiliation::Member,
                local_domain: "example.com".to_string(),
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
        message.id = Some(message_id.to_string());
        message.type_ = XmppMessageType::Groupchat;
        message.bodies.insert(
            String::new(),
            xmpp_parsers::message::Body("plain public-room message".to_string()),
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
    .expect("create room");
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
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");

    let mut mention = waddle_xmpp::xep::ExplicitMention::channel();
    mention.uri = Some("xmpp:other-channel@muc.example.com".to_string());
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid)));
    message.id = Some("foreign-channel-mention".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("not this room".to_string()),
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
    .expect("create room");
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
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("active-channel-push".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("heads up".to_string()),
    );
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
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: alice_jid.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::None,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join bob without durable affiliation");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid)));
    message.id = Some("active-channel-live-not-durable-push".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("heads up".to_string()),
    );
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
    .expect("create room");
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
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("active-channel-always-push".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("heads up".to_string()),
    );
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
    message.id = Some("offline-push-muted-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("do not push".to_string()),
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
    message.id = Some("storage-preserve-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("must-not-rollback".to_string()),
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
    message.id = Some("offline-push-transient-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("do not push transient".to_string()),
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
    message.id = Some("wire-recovery-resource-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("recover me".to_string()),
    );
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
            from: sender_bare.clone(),
            to: recipient.clone(),
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
    stanza_message.id = Some("wire-recovery-full-row-mismatched-stanza-1".to_string());
    stanza_message.type_ = XmppMessageType::Chat;
    stanza_message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("recover from full MAM row".to_string()),
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
    stanza_message.id = Some("wire-recovery-full-row-bare-stanza-1".to_string());
    stanza_message.type_ = XmppMessageType::Chat;
    stanza_message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("do not recover bare stanza sender".to_string()),
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
    stanza_message.id = Some("wire-recovery-mismatched-sender-1".to_string());
    stanza_message.type_ = XmppMessageType::Chat;
    stanza_message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("do not recover mismatched sender".to_string()),
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
    message.id = Some("dm-extension-spoof-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("spoofed extension".to_string()),
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
        responses[0].contains("from=\"bob@example.com/mobile\""),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to=\"alice@example.com/web\""),
        "response was {}",
        responses[0]
    );
}

#[tokio::test]
async fn handle_message_error_with_extension_envelope_does_not_emit_error_loop() {
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let recipient_jid: FullJid = "bob@example.com/mobile".parse().expect("recipient jid");
    let state = create_test_websocket_state().await;

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some("dm-extension-error-1".to_string());
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
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join alice");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("muc-extension-spoof-1".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("spoofed extension".to_string()),
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
        responses[0].contains("from=\"general@muc.example.com\""),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to=\"alice@example.com/web\""),
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
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bob_jid,
            nick: "bob".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
        })
        .await
        .expect("join bob");

    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room_jid.clone())));
    message.id = Some("muc-extension-non-occupant-1".to_string());
    message.type_ = XmppMessageType::Groupchat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("spoofed extension".to_string()),
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
        responses[0].contains("from=\"general@muc.example.com\""),
        "response was {}",
        responses[0]
    );
    assert!(
        responses[0].contains("to=\"alice@example.com/web\""),
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
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_jid.clone(), recipient_tx);

    let mut message =
        xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient_jid.clone())));
    message.id = Some("dm-extension-payload-1".to_string());
    message.type_ = XmppMessageType::Chat;
    message.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("Repo payload already attached".to_string()),
    );
    message.payloads.push(
        Element::builder("repo", "urn:waddle:test-extension:1")
            .attr("owner", "rust-lang")
            .attr("name", "rust")
            .attr("url", "xmpp:example.com?extension=test")
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
        if routed_xml.contains("type=\"chat\"") || routed_xml.contains("type='chat'") {
            assert!(
                routed_xml.contains("to=\"bob@example.com/mobile\"")
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
    state
        .deps
        .protocol
        .connection_registry
        .register(recipient_jid.clone(), recipient_tx);
    state
        .deps
        .protocol
        .connection_registry
        .register_with_carbons(sibling_jid.clone(), sibling_tx, true);

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
                <inbox xmlns='urn:xmpp:inbox:0'/>\
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
        entry_xml.contains("jid=\"alice@example.com\""),
        "streamed inbox entry should reference Alice: {entry_xml}"
    );
    assert!(
        entry_xml.contains("unread=\"1\""),
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
        fin_xml.contains("total=\"1\""),
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
        mark_read_xml.contains("type=\"result\""),
        "mark-read should succeed: {mark_read_xml}"
    );

    let unread_only_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-3'>\
                <inbox xmlns='urn:xmpp:inbox:0' unread-only='true'/>\
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
        unread_only_fin.contains("total=\"0\""),
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
    let frame = r#"<iq xmlns='jabber:client' type='get' to='bob@example.com' id='inbox-prebind-1'><inbox xmlns='urn:xmpp:inbox:0'/></iq>"#;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        state_machine: None,
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
    .await;

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
                <file-sharing xmlns='urn:xmpp:sfs:0'/>\
                <encrypted xmlns='urn:xmpp:esfs:0' cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                    <key>a2V5</key>\
                    <iv>aXY=</iv>\
                    <sources xmlns='urn:xmpp:sfs:0'>\
                        <url-data target='https://files.example.com/secret.enc'/>\
                    </sources>\
                </encrypted>\
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
                <inbox xmlns='urn:xmpp:inbox:0'/>\
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
        entry_xml.contains("jid=\"alice@example.com\""),
        "encrypted file-sharing inbox entry should target Alice: {entry_xml}"
    );
    assert!(
        entry_xml.contains("unread=\"1\""),
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
    state
        .deps
        .protocol
        .connection_registry
        .register(sender_jid.clone(), sender_tx);
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig::default(),
        waddle_id.to_string(),
        channel_id.to_string(),
    )
    .await
    .expect("create room");
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: "alice".to_string(),
            effective_affiliation: Affiliation::Member,
            local_domain: "example.com".to_string(),
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
            .any(|stanza| stanza.contains(&format!("to=\"{}\"", bob_jid))
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
    msg.id = Some("test-1".into());
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies
        .insert(String::new(), xmpp_parsers::message::Body("Hello".into()));

    let embed = xmpp_parsers::minidom::Element::builder("repo", "urn:waddle:test-extension:1")
        .attr("owner", "cuenv")
        .attr("name", "cuenv")
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
    msg.id = Some("test-2".into());
    msg.type_ = xmpp_parsers::message::MessageType::Chat;
    msg.bodies.insert(
        String::new(),
        xmpp_parsers::message::Body("No embeds".into()),
    );

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
    assert_eq!(parsed.id.as_deref(), Some("msg-1"));
    assert_eq!(
        parsed.thread.as_ref().map(|t| t.0.as_str()),
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
        reparsed.thread.as_ref().map(|thread| thread.0.as_str()),
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
    use xmpp_parsers::message::{Body, Message, MessageType, Thread};
    use xmpp_parsers::minidom::Element;

    let mut msg = Message::new(Some(jid::Jid::from(
        "bob@example.com".parse::<jid::BareJid>().expect("jid"),
    )));
    msg.from = Some(jid::Jid::from(
        "alice@example.com/web"
            .parse::<jid::FullJid>()
            .expect("jid"),
    ));
    msg.id = Some("msg-ns".to_string());
    msg.type_ = MessageType::Chat;
    msg.bodies.insert(String::new(), Body("hi".to_string()));
    msg.thread = Some(Thread("conversation-thread".to_string()));
    // Unrelated extension element happening to be named "thread"
    // in a different namespace — must not suppress reattachment.
    msg.payloads.push(
        Element::builder("thread", "urn:example:other:0")
            .attr("kind", "unrelated")
            .build(),
    );

    let rendered = stanza_to_xml(&Stanza::Message(msg));
    let reparsed = parse_message_for_test(&rendered);
    assert_eq!(
        reparsed.thread.as_ref().map(|t| t.0.as_str()),
        Some("conversation-thread"),
        "RFC 6121 thread must survive serialization despite unrelated <thread> in another ns; rendered: {rendered}"
    );
}
