//! IQ builders for extension-command invocation and form submission.
//!
//! Wire parity with the wasm chat client
//! (`chat/src/lib/xmpp/extension-commands/xml.ts`): submit forms
//! carry bare `var`/`value` fields — no XEP-0068 `FORM_TYPE`, no
//! per-field `type` attributes — and the `<x/>` element is omitted
//! entirely when there is nothing to submit.

use minidom::Element;

use crate::xep::xep0050::{
    build_xep0050_command_request_with_form_element, AdHocAction, NS_DATA_FORMS,
};

use super::types::ExtensionSubmitField;

/// Custom submit field carrying the MUC room a command is invoked
/// from (no XEP defines a room-context field for ad-hoc commands).
pub const ROOM_JID_FIELD: &str = "waddle#room_jid";

fn submit_form_element(fields: &[ExtensionSubmitField]) -> Option<Element> {
    if fields.is_empty() {
        return None;
    }
    let mut form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    for field in fields {
        let mut field_builder = Element::builder("field", NS_DATA_FORMS).attr(
            minidom::rxml::xml_ncname!("var").to_owned(),
            field.var.as_str(),
        );
        for value in &field.values {
            field_builder = field_builder.append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(value.as_str())
                    .build(),
            );
        }
        form = form.append(field_builder.build());
    }
    Some(form.build())
}

fn room_jid_field(room_jid: &str) -> ExtensionSubmitField {
    ExtensionSubmitField {
        var: ROOM_JID_FIELD.to_string(),
        values: vec![room_jid.to_string()],
    }
}

/// XEP-0050 §2.4 execute request that starts a command. When invoked
/// from a room the submit form carries the [`ROOM_JID_FIELD`].
pub fn build_extension_invoke_iq(service_jid: &str, node: &str, room_jid: Option<&str>) -> Element {
    let fields: Vec<ExtensionSubmitField> = room_jid.map(room_jid_field).into_iter().collect();
    build_xep0050_command_request_with_form_element(
        service_jid,
        node,
        None,
        AdHocAction::Execute,
        submit_form_element(&fields),
    )
}

/// Subsequent-stage submission. `cancel` and `prev` never carry a
/// form (the service replays or discards its own state); every other
/// action submits the palette's fields, with the [`ROOM_JID_FIELD`]
/// appended when the invocation came from a room and the palette did
/// not already carry it.
pub fn build_extension_submit_iq(
    service_jid: &str,
    node: &str,
    session_id: Option<&str>,
    action: AdHocAction,
    fields: &[ExtensionSubmitField],
    room_jid: Option<&str>,
) -> Element {
    let submitted: Vec<ExtensionSubmitField> =
        if matches!(action, AdHocAction::Cancel | AdHocAction::Prev) {
            Vec::new()
        } else {
            let mut submitted: Vec<ExtensionSubmitField> = fields.to_vec();
            if let Some(room_jid) = room_jid {
                if !submitted.iter().any(|field| field.var == ROOM_JID_FIELD) {
                    submitted.push(room_jid_field(room_jid));
                }
            }
            submitted
        };
    build_xep0050_command_request_with_form_element(
        service_jid,
        node,
        session_id,
        action,
        submit_form_element(&submitted),
    )
}
