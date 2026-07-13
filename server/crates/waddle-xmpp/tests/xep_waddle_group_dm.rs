use minidom::Element;
use waddle_xmpp::muc::{MucRoom, RoomConfig};
use waddle_xmpp::xep::xep_waddle_group_dm::{
    build_history_access, history_access_from_mediated_invite, GroupDmHistoryAccess, NS_GROUP_DM,
};

#[test]
fn group_dm_history_access_extension_round_trips_inside_mediated_invite() {
    let invite = Element::builder("invite", "http://jabber.org/protocol/muc#user")
        .append(build_history_access(GroupDmHistoryAccess::Full))
        .build();

    assert_eq!(
        history_access_from_mediated_invite(&invite),
        Some(GroupDmHistoryAccess::Full)
    );

    let child = invite
        .get_child("history-access", NS_GROUP_DM)
        .expect("history-access child");
    assert_eq!(child.attr("mode"), Some("full"));
}

#[test]
fn group_dm_history_access_defaults_to_from_join_when_mode_is_absent() {
    let invite: Element = format!(
        "<invite xmlns='http://jabber.org/protocol/muc#user'>\
            <history-access xmlns='{NS_GROUP_DM}'/>\
         </invite>"
    )
    .parse()
    .expect("valid invite");

    assert_eq!(
        history_access_from_mediated_invite(&invite),
        Some(GroupDmHistoryAccess::FromJoin)
    );
}

#[test]
fn group_dm_admission_is_always_members_only() {
    let room = MucRoom::new(
        "group@muc.example.com".parse().expect("room jid"),
        "waddle".to_string(),
        "channel".to_string(),
        RoomConfig {
            group_dm: true,
            members_only: false,
            ..RoomConfig::default()
        },
    );

    assert!(room.config.members_only);
    assert!(!room.can_user_join(&"stranger@example.com".parse().expect("stranger bare jid"),));
}
