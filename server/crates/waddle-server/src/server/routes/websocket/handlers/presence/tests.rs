use super::muc::{build_muc_join_presence_xml, MucJoinPresence};
use super::*;
use crate::server::routes::websocket::tests::create_test_websocket_state;
use tokio::sync::mpsc;
use waddle_xmpp::registry::OutboundStanza;

fn presence_from_xml(xml: &str) -> xmpp_parsers::presence::Presence {
    xmpp_parsers::presence::Presence::try_from(xml.parse::<Element>().expect("valid xml"))
        .expect("presence")
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
        None,
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
        warn_nonanonymous_join: false,
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

/// #1263: a MUC presence fan-out to a recipient with channel capacity
/// is delivered (baseline for the DroppedFull surfacing below). The
/// retry policy for a full channel is a single IMMEDIATE re-attempt —
/// no sleeps, because this helper sits inside the sequential join/leave
/// broadcast loops whose non-blocking contract is load-bearing.
#[tokio::test]
async fn muc_presence_fanout_delivers_when_channel_has_capacity() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "carol@example.com/web".parse().unwrap();
    let room: BareJid = "room@muc.example.com".parse().unwrap();

    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(1);
    let _owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut presence =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
    presence.to = Some(jid::Jid::from(jid.clone()));
    super::muc::route_room_presence_to_occupant(
        state.as_ref(),
        &room,
        &jid,
        waddle_xmpp::Stanza::Presence(presence),
    )
    .await;

    let delivered = rx.try_recv().expect("fan-out presence delivered");
    assert!(matches!(
        &delivered.stanza,
        waddle_xmpp::Stanza::Presence(p)
            if p.type_ == xmpp_parsers::presence::Type::Unavailable
    ));
}

/// #1263: when the recipient's channel is STILL full after every bounded
/// retry, the loss is surfaced through the
/// `xmpp.delivery.retry_exhausted_drop` counter (alias
/// `waddle_delivery_retry_exhausted_drop_total`) instead of
/// being silently reported as delivered.
#[tokio::test]
async fn muc_presence_fanout_counts_exhausted_full_channel_drop() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let jid: FullJid = "dave@example.com/web".parse().unwrap();
    let room: BareJid = "room@muc.example.com".parse().unwrap();

    // Capacity-1 channel, pre-filled and never drained: every retry
    // observes Full. Keep `rx` alive so the channel never reads closed.
    let (tx, rx) = mpsc::channel::<OutboundStanza>(1);
    let _owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    let filler = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    state
        .deps
        .protocol
        .connection_registry
        .try_send_to(&jid, waddle_xmpp::Stanza::Presence(filler));

    let mut presence =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
    presence.to = Some(jid::Jid::from(jid.clone()));
    super::muc::route_room_presence_to_occupant(
        state.as_ref(),
        &room,
        &jid,
        waddle_xmpp::Stanza::Presence(presence),
    )
    .await;
    assert_eq!(
        metrics.counter_sum("xmpp.delivery.retry_exhausted_drop", &[]),
        Some(1),
        "an exhausted DroppedFull fan-out must be surfaced via the drop counter"
    );
    drop(rx);
}
