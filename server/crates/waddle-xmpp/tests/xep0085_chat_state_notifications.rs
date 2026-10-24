//! XEP-0085: Chat State Notifications dedicated suite.

use waddle_xmpp::disco::{muc_room_features, server_features, Feature};

const CHAT_STATES_NS: &str = "http://jabber.org/protocol/chatstates";

#[test]
fn xep0085_advertisement_consistency_no_false_feature_claim() {
    let features = server_features();
    assert!(!features.contains(&Feature::chat_states()));
}

#[test]
fn xep0085_muc_rooms_advertise_chat_states() {
    let features = muc_room_features(
        waddle_xmpp_core::disco::info::RoomPersistence::Temporary,
        waddle_xmpp_core::disco::info::RoomMembership::Open,
        waddle_xmpp_core::disco::info::RoomVisibility::Public,
        waddle_xmpp_core::types::Moderation::Unmoderated,
    );
    assert!(features.contains(&Feature::chat_states()));
    assert_eq!(Feature::chat_states(), Feature::new(CHAT_STATES_NS));
}
