use super::*;

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
    let push_service_jid: BareJid = "push.example.com".parse().expect("push service jid");
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
    // `to` attribute. The dispatcher path preserves this; legacy
    // `handle_message` rewrote `to` to the per-resource full JID,
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

    let inbox_query = format!(
        "<iq xmlns='jabber:client' type='get' to='{}' id='inbox-1'>\
                <query xmlns='urn:waddle:inbox:0'/>\
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
    let inbox_xml = inbox_responses.first().expect("inbox response");
    assert!(
        inbox_xml.contains("partner=\"alice@example.com\""),
        "inbox response should include Alice conversation: {inbox_xml}"
    );
    assert!(
        inbox_xml.contains("unread=\"1\""),
        "inbox response should report one unread DM: {inbox_xml}"
    );
    assert!(
        inbox_xml.contains("<preview>Hello from Alice</preview>"),
        "inbox response should include preview text: {inbox_xml}"
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
                <query xmlns='urn:waddle:inbox:0' only-unread='true'/>\
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
    let unread_only_xml = unread_only_responses.first().expect("unread-only response");
    assert!(
        unread_only_xml.contains("total-unread=\"0\""),
        "mark-read should clear the unread count: {unread_only_xml}"
    );
    assert!(
        !unread_only_xml.contains("<conversation "),
        "unread-only query should be empty after mark-read: {unread_only_xml}"
    );
}

#[tokio::test]
async fn inbox_query_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let pending_jid: FullJid = "bob@example.com/pending".parse().expect("pending jid");
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let frame = r#"<iq xmlns='jabber:client' type='get' to='bob@example.com' id='inbox-prebind-1'><query xmlns='urn:waddle:inbox:0'/></iq>"#;
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
                <query xmlns='urn:waddle:inbox:0'/>\
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
    let inbox_xml = inbox_responses.first().expect("inbox response");
    assert!(
        inbox_xml.contains("partner=\"alice@example.com\""),
        "encrypted file-sharing inbox entry should target Alice: {inbox_xml}"
    );
    assert!(
        inbox_xml.contains("unread=\"1\""),
        "encrypted file-sharing inbox entry should increment unread: {inbox_xml}"
    );
    assert!(
        !inbox_xml.contains("<preview>"),
        "bodyless encrypted file-sharing message should not invent preview text: {inbox_xml}"
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
