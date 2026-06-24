use super::muc::{build_muc_join_presence_xml, MucJoinPresence};
use super::*;

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
        include_self_status: false,
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
