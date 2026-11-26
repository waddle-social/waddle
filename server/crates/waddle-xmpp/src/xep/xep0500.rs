//! XEP-0500: MUC Slow Mode
//!
//! Only the disco#info advertisement is implemented. Every room reports
//! `muc#roominfo_slow_mode_duration` as `0` (slow mode disabled) inside its
//! single `muc#roominfo` form (see `crate::muc::roominfo::MucRoomInfo`).
//!
//! There is no `muc#roomconfig_slow_mode_duration` room configuration field
//! and no per-occupant rate limiting in MUC send authorization, so slow mode
//! cannot be enabled.

use minidom::Element;

use super::xep0004::{Field, ToElement};

/// MUC roominfo disco field for slow mode duration.
pub const FIELD_ROOMINFO_SLOW_MODE_DURATION: &str = "muc#roominfo_slow_mode_duration";

/// Build the XEP-0500 roominfo field for disco#info extension forms.
///
/// The field is emitted inside the room's single `muc#roominfo` form —
/// see `crate::muc::roominfo::MucRoomInfo`. XEP-0500 must not append
/// its own `muc#roominfo` form: two forms with the same FORM_TYPE make
/// the disco#info response ill-formed per XEP-0115 §5.4 (#1259).
pub fn build_roominfo_slow_mode_duration_field(duration_secs: u64) -> Element {
    Field::text_single(FIELD_ROOMINFO_SLOW_MODE_DURATION, duration_secs.to_string()).to_element()
}

#[cfg(test)]
mod tests {
    use super::super::xep0004::NS_DATA_FORMS;
    use super::*;

    #[test]
    fn test_build_roominfo_slow_mode_duration_field() {
        let field = build_roominfo_slow_mode_duration_field(20);
        assert_eq!(field.name(), "field");
        assert_eq!(field.ns(), NS_DATA_FORMS);
        assert_eq!(field.attr("var"), Some(FIELD_ROOMINFO_SLOW_MODE_DURATION));
        assert_eq!(field.attr("type"), Some("text-single"));
        assert_eq!(
            field.get_child("value", NS_DATA_FORMS).map(|v| v.text()),
            Some("20".to_owned())
        );
    }
}
