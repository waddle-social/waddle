//! XEP-0045 / XEP-0068: the owner configuration form (FORM_TYPE
//! `http://jabber.org/protocol/muc#roomconfig`) is managed by the XSF, so every
//! field Waddle adds to it must be namespaced with Clark notation
//! (XEP-0068 §2) and must not borrow the `muc#roomconfig_` prefix.

use waddle_xmpp::muc::{build_config_form, MucRoom, RoomConfig, DATA_FORMS_NS};

/// `muc#roomconfig_*` fields registered by XEP-0045 (and the XSF registry) that
/// Waddle's form may legitimately use.
const REGISTERED_ROOMCONFIG_FIELDS: &[&str] = &[
    "muc#roomconfig_roomname",
    "muc#roomconfig_roomdesc",
    "muc#roomconfig_persistentroom",
    "muc#roomconfig_publicroom",
    "muc#roomconfig_membersonly",
    "muc#roomconfig_moderatedroom",
    "muc#roomconfig_maxusers",
    "muc#roomconfig_enablelogging",
    "muc#roomconfig_changesubject",
    "muc#roomconfig_allowpm",
    "muc#roomconfig_roomsecret",
    "muc#roomconfig_passwordprotectedroom",
    "muc#roomconfig_whois",
    "muc#roomconfig_allowinvites",
    "muc#roomconfig_pubsub",
    "muc#roomconfig_lang",
    "muc#roomconfig_getmemberlist",
    "muc#roomconfig_presencebroadcast",
    "muc#roomconfig_roomadmins",
    "muc#roomconfig_roomowners",
    "muc#maxhistoryfetch",
];

fn form_vars() -> Vec<String> {
    let room = MucRoom::new(
        "room@muc.example.com".parse().expect("room jid"),
        "waddle-123".to_string(),
        "channel-456".to_string(),
        RoomConfig::default(),
    );
    build_config_form(&room)
        .children()
        .filter(|c| c.is("field", DATA_FORMS_NS))
        .filter_map(|c| c.attr("var").map(str::to_owned))
        .collect()
}

#[test]
fn waddle_roomconfig_fields_use_clark_notation() {
    let vars = form_vars();
    for var in [
        "{urn:waddle:roomconfig}forum",
        "{urn:waddle:roomconfig}pinpermission",
        "{urn:waddle:roomconfig}federated",
    ] {
        assert!(vars.iter().any(|v| v == var), "missing {var} in {vars:?}");
    }
}

#[test]
fn every_unregistered_roomconfig_field_is_clark_namespaced() {
    for var in form_vars() {
        if var == "FORM_TYPE" || REGISTERED_ROOMCONFIG_FIELDS.contains(&var.as_str()) {
            continue;
        }
        assert!(
            var.starts_with("{urn:waddle:") && var.contains('}'),
            "`{var}` is neither a registered field nor Clark-namespaced (XEP-0068 §2)"
        );
    }
}
