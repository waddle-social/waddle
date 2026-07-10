//! XEP-0045 §6.4 `muc#roominfo` disco#info extension form.
//!
//! A disco#info response may carry at most ONE extension form per
//! FORM_TYPE — duplicate FORM_TYPE forms are ill-formed per XEP-0115
//! §5.4 / XEP-0128 and force caps-verifying clients to discard the
//! whole response (#1259). Every `muc#roominfo` field a room wants to
//! expose (description, XEP-0500 slow-mode duration, XEP-0503 space
//! link) therefore flows through this single builder instead of each
//! XEP module appending its own form.

use minidom::Element;

use crate::xep::xep0004::{DataForm, Field, FormType, ToElement};
use crate::xep::xep0500::FIELD_ROOMINFO_SLOW_MODE_DURATION;

/// FORM_TYPE of the XEP-0045 §6.4 room-information extension form.
pub const FORM_TYPE_MUC_ROOMINFO: &str = "http://jabber.org/protocol/muc#roominfo";

/// XEP-0045 registrar field carrying the room description.
pub const FIELD_ROOMINFO_DESCRIPTION: &str = "muc#roominfo_description";

/// XEP-0503 compatibility field linking the room to its parent space
/// pubsub node.
pub const FIELD_ROOMCONFIG_PUBSUB: &str = "muc#roomconfig_pubsub";

/// Typed contents of a room's single `muc#roominfo` extension form.
#[derive(Debug, Clone, Default)]
pub struct MucRoomInfo {
    /// Room description (`muc#roominfo_description`), omitted when
    /// empty/absent.
    pub description: Option<String>,
    /// XEP-0500 slow-mode interval in seconds (0 = disabled; always
    /// emitted so clients can distinguish "disabled" from "unknown").
    pub slow_mode_duration_secs: u64,
    /// XEP-0503 parent-space pubsub node IRI
    /// (`muc#roomconfig_pubsub`), omitted when the room is not linked
    /// to a space.
    pub space_pubsub_iri: Option<String>,
}

impl MucRoomInfo {
    /// Build the single `muc#roominfo` result form for disco#info.
    pub fn to_form_element(&self) -> Element {
        let mut form =
            DataForm::new(FormType::Result).add_field(Field::form_type(FORM_TYPE_MUC_ROOMINFO));
        if let Some(description) = self
            .description
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            form = form.add_field(Field::text_single(FIELD_ROOMINFO_DESCRIPTION, description));
        }
        form = form.add_field(Field::text_single(
            FIELD_ROOMINFO_SLOW_MODE_DURATION,
            self.slow_mode_duration_secs.to_string(),
        ));
        if let Some(iri) = self.space_pubsub_iri.as_deref() {
            form = form.add_field(Field::text_single(FIELD_ROOMCONFIG_PUBSUB, iri));
        }
        form.to_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_values(form: &Element, var: &str) -> Vec<String> {
        form.children()
            .filter(|child| child.name() == "field" && child.attr("var") == Some(var))
            .map(|field| {
                field
                    .children()
                    .filter(|c| c.name() == "value")
                    .flat_map(|c| c.texts())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn minimal_form_has_single_form_type_and_slow_mode() {
        let form = MucRoomInfo::default().to_form_element();
        assert_eq!(
            field_values(&form, "FORM_TYPE"),
            vec![FORM_TYPE_MUC_ROOMINFO.to_string()]
        );
        assert_eq!(
            field_values(&form, FIELD_ROOMINFO_SLOW_MODE_DURATION),
            vec!["0".to_string()]
        );
        assert!(field_values(&form, FIELD_ROOMINFO_DESCRIPTION).is_empty());
        assert!(field_values(&form, FIELD_ROOMCONFIG_PUBSUB).is_empty());
    }

    #[test]
    fn full_form_carries_description_and_space_link_in_one_form() {
        let info = MucRoomInfo {
            description: Some("A cosy room".to_string()),
            slow_mode_duration_secs: 20,
            space_pubsub_iri: Some("xmpp:spaces.example?;node=eng".to_string()),
        };
        let form = info.to_form_element();
        assert_eq!(
            field_values(&form, "FORM_TYPE"),
            vec![FORM_TYPE_MUC_ROOMINFO.to_string()]
        );
        assert_eq!(
            field_values(&form, FIELD_ROOMINFO_DESCRIPTION),
            vec!["A cosy room".to_string()]
        );
        assert_eq!(
            field_values(&form, FIELD_ROOMINFO_SLOW_MODE_DURATION),
            vec!["20".to_string()]
        );
        assert_eq!(
            field_values(&form, FIELD_ROOMCONFIG_PUBSUB),
            vec!["xmpp:spaces.example?;node=eng".to_string()]
        );
    }

    #[test]
    fn blank_description_is_omitted() {
        let info = MucRoomInfo {
            description: Some("   ".to_string()),
            ..MucRoomInfo::default()
        };
        let form = info.to_form_element();
        assert!(field_values(&form, FIELD_ROOMINFO_DESCRIPTION).is_empty());
    }
}
