//! XEP-0333: Displayed Markers dedicated suite.

use jid::Jid;
use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::{add_markable, extract_marker_from_message, has_markable, Marker};
use xmpp_parsers::message::Message;

#[test]
fn xep0333_server_and_muc_disco_advertise_displayed_markers() {
    let server = server_features();
    assert!(server.contains(&Feature::chat_markers()));

    let room = muc_room_features(
        waddle_xmpp_core::disco::info::RoomPersistence::Temporary,
        waddle_xmpp_core::disco::info::RoomMembership::Open,
        waddle_xmpp_core::disco::info::RoomVisibility::Public,
        waddle_xmpp_core::types::Moderation::Unmoderated,
    );
    assert!(room.contains(&Feature::chat_markers()));
}

#[test]
fn xep0333_markable_requires_message_id_for_traceability() {
    let to: Jid = "peer@localhost".parse().expect("valid jid");
    let mut msg = Message::new(Some(to));
    add_markable(&mut msg);

    assert!(!has_markable(&msg));
    assert_eq!(extract_marker_from_message(&msg), None);
}

#[test]
fn xep0333_markable_with_message_id_is_detected() {
    let to: Jid = "peer@localhost".parse().expect("valid jid");
    let mut msg = Message::new(Some(to));
    msg.id = Some(xmpp_parsers::message::Id("msg-1".to_string()));
    add_markable(&mut msg);

    assert!(has_markable(&msg));
    assert_eq!(extract_marker_from_message(&msg), Some(Marker::Markable));
}

#[test]
fn xep0333_acknowledged_marker_reads_like_displayed() {
    let xml = "<message xmlns='jabber:client' type='chat'>\
                <acknowledged xmlns='urn:xmpp:chat-markers:0' id='msg-9'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<minidom::Element>().expect("valid xml"))
        .expect("valid message");

    let marker = extract_marker_from_message(&msg).expect("acknowledged marker");
    assert_eq!(marker, Marker::Acknowledged("msg-9".to_owned()));
    assert_eq!(marker.read_through_id(), Some("msg-9"));
    assert_eq!(
        Marker::Displayed("msg-9".to_owned()).read_through_id(),
        Some("msg-9")
    );
    assert_eq!(Marker::Markable.read_through_id(), None);
}

#[test]
fn xep0333_marker_without_an_id_references_no_message() {
    let xml = "<message xmlns='jabber:client' type='chat'>\
                <acknowledged xmlns='urn:xmpp:chat-markers:0'/>\
                </message>";
    let msg = Message::try_from(xml.parse::<minidom::Element>().expect("valid xml"))
        .expect("valid message");

    assert_eq!(extract_marker_from_message(&msg), None);
}
