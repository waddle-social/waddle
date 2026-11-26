//! XEP-0500: MUC Slow Mode — dedicated suite.
//!
//! Pins the disco#info advertisement: the roominfo field var and its
//! placement in the room's single `muc#roominfo` form (always `0`, since
//! slow mode cannot be enabled).
//!
use waddle_xmpp::muc::roominfo::MucRoomInfo;
use waddle_xmpp::xep::xep0004::NS_DATA_FORMS;
use waddle_xmpp::xep::xep0500::{
    build_roominfo_slow_mode_duration_field, FIELD_ROOMINFO_SLOW_MODE_DURATION,
};

#[test]
fn xep0500_roominfo_field_constant() {
    assert_eq!(
        FIELD_ROOMINFO_SLOW_MODE_DURATION,
        "muc#roominfo_slow_mode_duration"
    );
}

#[test]
fn xep0500_roominfo_field_uses_spec_var() {
    let field = build_roominfo_slow_mode_duration_field(20);
    assert_eq!(field.name(), "field");
    assert_eq!(field.ns(), NS_DATA_FORMS);
    assert_eq!(field.attr("var"), Some(FIELD_ROOMINFO_SLOW_MODE_DURATION));
    assert_eq!(field.attr("type"), Some("text-single"));
    assert_eq!(
        field
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text()),
        Some("20".to_owned())
    );
}

#[test]
fn xep0500_roominfo_form_carries_duration_field() {
    // The duration field rides in the room's single `muc#roominfo`
    // form (#1259: one FORM_TYPE per disco#info response).
    let form = MucRoomInfo::default().to_form_element();
    assert_eq!(form.name(), "x");
    assert_eq!(form.ns(), NS_DATA_FORMS);
    assert_eq!(form.attr("type"), Some("result"));
    let field = form
        .children()
        .find(|child| child.attr("var") == Some(FIELD_ROOMINFO_SLOW_MODE_DURATION))
        .expect("slow mode roominfo field");
    assert_eq!(
        field
            .get_child("value", NS_DATA_FORMS)
            .map(|value| value.text()),
        Some("0".to_owned())
    );
}
