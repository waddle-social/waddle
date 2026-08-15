use super::frame::ResponseFrame;
use super::*;

fn build_bind_result_element(id: &str, full_jid: &FullJid) -> Element {
    Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("bind", waddle_xmpp::ns::BIND)
                .append(
                    Element::builder("jid", waddle_xmpp::ns::BIND)
                        .append(full_jid.to_string())
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Handle resource binding IQ.
pub(super) fn handle_resource_binding(
    iq: &xmpp_parsers::iq::Iq,
    _domain: &str,
    phase: &mut ConnectionPhase,
) -> Vec<ResponseFrame> {
    let id = iq.id().to_string();

    if !phase.allows_resource_binding() {
        warn!(phase = ?phase, id = %id, "Resource binding received in invalid phase");
        return vec![ResponseFrame::from(build_iq_error_xml_typed(
            &id,
            None,
            None,
            not_authorized_iq_error("Authentication required."),
        ))];
    }

    let Some(bare_jid) = phase.authenticated_bare_jid() else {
        warn!(id = %id, "Resource binding without authenticated session");
        return vec![ResponseFrame::from(build_iq_error_xml_typed(
            &id,
            None,
            None,
            not_authorized_iq_error("Authentication required."),
        ))];
    };
    let bare_jid = bare_jid.clone();
    let resource = match iq {
        xmpp_parsers::iq::Iq::Set { payload: e, .. }
        | xmpp_parsers::iq::Iq::Get { payload: e, .. } => e
            .get_child("resource", waddle_xmpp::ns::BIND)
            .map(|r| r.text().trim().to_string())
            .filter(|v| !v.is_empty()),
        _ => None,
    }
    .unwrap_or_else(|| format!("ws-{}", uuid::Uuid::new_v4()));

    let full_jid_str = format!("{}/{}", bare_jid, resource);

    if let Ok(full_jid) = full_jid_str.parse::<FullJid>() {
        info!(jid = %full_jid, id = %id, "Resource bound");
        *phase = ConnectionPhase::ready(full_jid.clone(), false);
        vec![ResponseFrame::from(build_bind_result_element(
            &id, &full_jid,
        ))]
    } else {
        warn!(jid = %full_jid_str, "Invalid JID during resource binding");
        vec![]
    }
}
