use super::muc::{build_muc_join_presence_xml, MucJoinPresence};
use super::*;
use crate::server::routes::websocket::tests::create_test_websocket_state;
use tokio::sync::mpsc;
use waddle_xmpp::registry::OutboundStanza;

fn presence_from_xml(xml: &str) -> xmpp_parsers::presence::Presence {
    xmpp_parsers::presence::Presence::try_from(xml.parse::<Element>().expect("valid xml"))
        .expect("presence")
}

fn pending_transient_row(
    recipient: &BareJid,
    from: &str,
    body: &str,
) -> waddle_xmpp::pending_delivery::PendingRow {
    let mut message = xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
    message.from = Some(from.parse::<jid::Jid>().expect("from jid"));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message
        .bodies
        .insert(xmpp_parsers::message::Lang(String::new()), body.to_string());
    waddle_xmpp::pending_delivery::PendingRow {
        id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at: chrono::Utc::now(),
        payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new(message)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

/// Issue #1220 regression: the XEP-0160 pending-delivery flush must NOT
/// run inline on the recipient's own connection task. A recipient with
/// more queued rows than the 256-slot outbound channel capacity used to
/// self-deadlock on login — the inline flush's blocking sends filled the
/// mpsc that only this same task drains, so the presence handler never
/// returned. The flush is now spawned off-task, so the handler returns
/// promptly and the connection keeps draining while the flush
/// backpressures naturally on the channel.
#[tokio::test]
async fn pending_flush_exceeding_outbound_channel_cap_does_not_wedge_the_handler() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().unwrap();
    let recipient = jid.to_bare();

    // Queue more offline rows than the real outbound channel capacity
    // (`OUTBOUND_CHANNEL_SIZE = 256`).
    const TOTAL: usize = 300;
    for n in 0..TOTAL {
        state
            .deps
            .protocol
            .pending_delivery_storage
            .insert(pending_transient_row(
                &recipient,
                "bob@elsewhere/x",
                &format!("queued-{n}"),
            ))
            .await
            .expect("insert queued row");
    }

    // Register the recovering connection with a real 256-slot channel,
    // exactly as the live connection loop does. The test task is the
    // sole consumer of `rx` — mirroring the connection task that both
    // runs the presence handler and drains its own outbound channel.
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(256);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    // First available presence with non-negative priority triggers the
    // flush. Against the pre-fix inline flush this call never returns
    // (it wedges filling the undrained 256-slot channel); the timeout
    // turns that wedge into a clean failure.
    let presence =
        presence_from_xml("<presence xmlns='jabber:client'><priority>1</priority></presence>");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        super::regular::handle_regular_presence_update(
            state.as_ref(),
            &jid,
            Some(&owner),
            presence,
        ),
    )
    .await
    .expect("presence handler must not block on the pending-delivery flush (issue #1220)");

    // Drain the connection's outbound channel; every queued row must
    // arrive without the connection wedging.
    let mut delivered = 0usize;
    while delivered < TOTAL {
        let outbound = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("flush must keep making progress once the task drains")
            .expect("outbound channel stays open for the whole flush");
        if matches!(outbound.stanza, waddle_xmpp::Stanza::Message(_)) {
            delivered += 1;
        }
    }
    assert_eq!(delivered, TOTAL, "every queued offline row is delivered");
}

/// Issue #1208: a superseded same-full-JID connection processing a late
/// available presence must not stamp its stale presence onto the
/// replacement's registry entry, and must not consume the replacement's
/// once-per-session pending-subscribe / offline-flush claims.
#[tokio::test]
async fn superseded_connection_presence_does_not_touch_replacement_registry_state() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().unwrap();
    let registry = &state.deps.protocol.connection_registry;

    // Connection A registers, then a same-JID replacement B supersedes it.
    let (tx_a, _rx_a) = mpsc::channel::<OutboundStanza>(4);
    let stale_owner = registry.register(jid.clone(), tx_a);
    let (tx_b, _rx_b) = mpsc::channel::<OutboundStanza>(4);
    let live_owner = registry.register(jid.clone(), tx_b);

    // B publishes its own presence.
    assert!(registry.update_presence_if_owner(&jid, &live_owner, true, 3));
    assert!(registry.update_presence_state_if_owner(
        &jid,
        &live_owner,
        Some("dnd".to_string()),
        Some("busy".to_string()),
        3,
        Vec::new(),
    ));

    // A's late available frame is processed with A's stale owner token.
    let stale_presence = presence_from_xml(
        "<presence xmlns='jabber:client'><show>xa</show>\
         <status>stale</status><priority>7</priority></presence>",
    );
    super::regular::handle_regular_presence_update(
        state.as_ref(),
        &jid,
        Some(&stale_owner),
        stale_presence,
    )
    .await;

    // B's presence entry and state survive untouched.
    let entry = registry.get_entry(&jid).expect("replacement entry");
    assert_eq!(
        entry.presence_priority(),
        3,
        "a superseded connection must not overwrite the replacement's priority"
    );
    let presence_state = registry
        .get_presence_state(&jid)
        .expect("replacement presence state");
    assert_eq!(presence_state.show.as_deref(), Some("dnd"));
    assert_eq!(presence_state.status.as_deref(), Some("busy"));
    assert_eq!(presence_state.priority, 3);

    // B's once-per-session claims are still unconsumed.
    assert!(
        entry.claim_pending_subscribes_flush(),
        "a superseded connection must not consume the replacement's pending-subscribe claim"
    );
    assert!(
        entry.claim_offline_flush(),
        "a superseded connection must not consume the replacement's offline-flush claim"
    );
}

#[test]
fn muc_join_presence_carries_authority_in_xep_0045_payload_only() {
    // XEP-0317 §1: hats are descriptive social metadata, not a
    // duplicate of authority. Owner / admin / moderator status
    // belongs in the XEP-0045 `<x xmlns='muc#user'><item …/>`
    // payload and MUST NOT be synthesised as `<hat/>` entries.
    //
    // This builder is the on-the-wire shape for a fresh-join
    // presence, so the assertion here pins both directions: the
    // XEP-0045 payload IS emitted, and the XEP-0317 payload is NOT.
    let secret = waddle_xmpp::xep::xep0421::OccupantIdSecret::new(
        b"join-presence-handler-test-secret".to_vec(),
    )
    .expect("test secret meets length floor");
    let room_jid: BareJid = "chat@muc.example.com".parse().unwrap();
    let to_jid: FullJid = "alice@example.com/web".parse().unwrap();
    let real_jid: FullJid = "bob@example.com/mobile".parse().unwrap();

    let xml = build_muc_join_presence_xml(MucJoinPresence {
        occupant_id_secret: &secret,
        room_jid: &room_jid,
        nick: "bob",
        to_jid: &to_jid,
        affiliation: Affiliation::Owner,
        role: Role::Moderator,
        real_jid: &real_jid,
        disclose_real_jid: true,
        include_self_status: false,
        room_created: false,
        include_nonanonymous_status: true,
        muji: None,
        in_call: waddle_xmpp::xep::InCallPresenceState::default(),
    });

    // XEP-0045: authority lives in the muc#user payload.
    assert!(
        xml.contains("xmlns='http://jabber.org/protocol/muc#user'")
            || xml.contains("xmlns='http://jabber.org/protocol/muc#user'"),
        "join presence must carry the XEP-0045 muc#user payload: {xml}"
    );
    assert!(
        xml.contains("affiliation='owner'") || xml.contains("affiliation='owner'"),
        "join presence must declare affiliation in muc#user item: {xml}"
    );
    assert!(
        xml.contains("role='moderator'") || xml.contains("role='moderator'"),
        "join presence must declare role in muc#user item: {xml}"
    );

    // XEP-0421: occupant-id MUST be stamped on every MUC presence.
    assert!(
        xml.contains("<occupant-id") && xml.contains("urn:xmpp:occupant-id:0"),
        "typed join presence builder must stamp XEP-0421 occupant-id: {xml}"
    );
}

#[test]
fn rebuilt_available_presence_carries_xep0319_idle_for_subscribers() {
    // The presence relay rebuilds an available stanza from show/status/priority,
    // which drops payloads; the broadcast (`regular.rs`) and probe (`delivery.rs`)
    // paths re-attach the XEP-0319 idle stamp so a subscriber sees the contact's
    // idle age rather than a bare away dot. This pins that rebuild+carry shape.
    let from: FullJid = "alice@example.com/web".parse().unwrap();
    let to: BareJid = "bob@example.com".parse().unwrap();
    let since = "2024-06-01T12:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .expect("valid xs:dateTime");

    let mut presence = build_available_presence(&from, &to, Some("away"), None, 0);
    waddle_xmpp::xep::xep0319::add_idle(&mut presence, since);

    // The rebuilt presence keeps the Show and carries the typed idle instant.
    assert_eq!(presence.show, Some(xmpp_parsers::presence::Show::Away));
    let idle = waddle_xmpp::xep::xep0319::extract_idle_from_presence(&presence)
        .expect("rebuilt away presence carries an <idle/> stamp");
    assert_eq!(idle.since, since);

    // Byte-conformant XEP-0319: an `<idle xmlns='urn:xmpp:idle:1' since='…'/>`.
    let element = Element::from(presence);
    let idle_el = element
        .children()
        .find(|child| child.name() == "idle")
        .expect("serialized presence has an <idle/> child");
    assert_eq!(idle_el.ns(), "urn:xmpp:idle:1");
    assert!(
        idle_el
            .attr("since")
            .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
            == Some(since),
        "idle since serializes to the stamped xs:dateTime"
    );
}
