use super::frame::ResponseFrame;
use super::handlers::iq::errors::resource_constraint_iq_error;
use super::{
    build_iq_error_xml_typed, info, not_authorized_iq_error, warn, ConnectionPhase, Element,
    FullJid,
};

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
    resume_endpoint_handoff_blocked: bool,
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
    // Registration cannot publish a fresh binding while the old endpoint
    // still owns accepted frames from a failed resume.
    if resume_endpoint_handoff_blocked {
        return vec![ResponseFrame::from(build_iq_error_xml_typed(
            &id,
            None,
            None,
            resource_constraint_iq_error("Reconnect before binding a resource."),
        ))];
    }

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
        vec![ResponseFrame::from(build_iq_error_xml_typed(
            &id,
            None,
            None,
            super::handlers::iq::errors::bad_request_iq_error(
                "The requested resourcepart is invalid.",
            ),
        ))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::bind::BindQuery;
    use xmpp_parsers::iq::Iq;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    #[test]
    fn blocked_bind_returns_correlated_resource_constraint_without_changing_phase() {
        let jid: FullJid = "alice@example.com/web".parse().expect("JID");
        let mut phase = ConnectionPhase::authenticated(&jid);
        let request = Iq::from_set("blocked-bind", BindQuery::new(Some("web".to_owned())));

        let mut responses = handle_resource_binding(&request, "example.com", &mut phase, true);

        assert_eq!(responses.len(), 1);
        let response: Element = responses
            .pop()
            .expect("bind response")
            .into_serialized_xml()
            .parse()
            .expect("response XML");
        assert!(response.is("iq", waddle_xmpp::ns::JABBER_CLIENT));
        assert_eq!(response.attr("id"), Some("blocked-bind"));
        assert_eq!(response.attr("type"), Some("error"));
        let error = StanzaError::try_from(
            response
                .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
                .expect("stanza error")
                .clone(),
        )
        .expect("typed error");
        assert_eq!(error.type_, ErrorType::Wait);
        assert_eq!(
            error.defined_condition,
            DefinedCondition::ResourceConstraint
        );
        assert!(matches!(phase, ConnectionPhase::Authenticated { .. }));
        assert_eq!(phase.authenticated_bare_jid(), Some(&jid.to_bare()));
    }

    #[test]
    fn unblocked_bind_returns_correlated_result_and_becomes_ready() {
        let jid: FullJid = "alice@example.com/web".parse().expect("JID");
        let mut phase = ConnectionPhase::authenticated(&jid);
        let request = Iq::from_set("fresh-bind", BindQuery::new(Some("web".to_owned())));

        let mut responses = handle_resource_binding(&request, "example.com", &mut phase, false);

        assert_eq!(responses.len(), 1);
        let response: Element = responses
            .pop()
            .expect("bind response")
            .into_serialized_xml()
            .parse()
            .expect("response XML");
        assert!(response.is("iq", waddle_xmpp::ns::JABBER_CLIENT));
        assert_eq!(response.attr("id"), Some("fresh-bind"));
        assert_eq!(response.attr("type"), Some("result"));
        let bound = response
            .get_child("bind", waddle_xmpp::ns::BIND)
            .and_then(|bind| bind.get_child("jid", waddle_xmpp::ns::BIND))
            .expect("bound JID");
        assert_eq!(bound.text(), "alice@example.com/web");
        assert!(phase.is_ready());
        assert_eq!(phase.bound_jid(), Some(&jid));
    }
}
