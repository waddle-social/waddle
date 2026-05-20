//! XEP-0333: Displayed Markers dedicated suite.

use jid::Jid;
use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::{add_markable, extract_marker_from_message, has_markable, Marker};
use xmpp_parsers::message::Message;

#[test]
fn xep0333_server_and_muc_disco_advertise_displayed_markers() {
    let server = server_features();
    assert!(server.contains(&Feature::chat_markers()));

    let room = muc_room_features(false, false, false, false);
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
