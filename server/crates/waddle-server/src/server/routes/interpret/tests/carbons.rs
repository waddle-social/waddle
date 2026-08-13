use super::*;

// -----------------------------------------------------------------
// XEP-0280 — SendCarbons fan-out
// -----------------------------------------------------------------

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
    let original = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
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
    let original = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
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
    let original = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
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
    let original = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
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
        shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
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
    let original = chat_msg(jid("alice@example.com/web"), jid("bob@example.com"), "hi");
    let deps = Deps {
        connection_registry: &registry,
        user_registry: None,
        sm_session_registry: Some(&sm),
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
        ingress_effect_capture: None,
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
