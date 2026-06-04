use minidom::Element;

use crate::event::{ClientEvent, ConnectionEvent};
use crate::transport::TransportMessage;
use crate::AuthenticationConfig;

use super::XmppRuntime;

impl XmppRuntime {
    /// Route a post-bootstrap application stanza to a typed [`ClientEvent`].
    ///
    /// IQ result and error stanzas are returned as [`ClientEvent::IqResult`] so the
    /// driver can route them to its IQ correlation map without broadcasting them on
    /// the public event bus.  Message stanzas are dispatched to the typed protocol
    /// handlers in priority order: MAM results, PEP events, calls, then general
    /// messaging. Unrecognised stanzas fall through to
    /// [`ClientEvent::UnhandledStanza`].
    pub fn handle_app_stanza(&mut self, element: &minidom::Element) -> Vec<ClientEvent> {
        use crate::{caps, inbox, mam, messaging, pep};

        let type_attr = element.attr("type").unwrap_or("");

        if element.name() == "iq" && (type_attr == "result" || type_attr == "error") {
            if let Some(id) = element.attr("id") {
                return vec![ClientEvent::IqResult {
                    id: id.to_string(),
                    element: element.clone(),
                }];
            }
        }

        if let Some(response) = caps::build_client_caps_disco_info_response(
            element,
            self.snapshot.binding.as_ref().map(|binding| &binding.jid),
        ) {
            return vec![ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                TransportMessage::Element(response),
            ))];
        }

        if element.name() == "message" {
            // XEP-0430 entries take priority over the MAM parser: query
            // responses may embed a MAM `<result/>` (when
            // `messages='true'`), while Waddle live pushes wrap the entry
            // in a private `<push/>` marker. Either way, the inbox entry
            // is the state transition the driver must see first.
            if let Some(entry) = inbox::parse_inbox_stream_message(element) {
                return vec![ClientEvent::InboxStreamEntry(entry)];
            }

            if let Some(archived) = mam::parse_mam_result(element) {
                return vec![ClientEvent::MamResult(Box::new(archived))];
            }

            if let Some(pep_item) = pep::parse(element) {
                return vec![ClientEvent::PepEvent(pep_item)];
            }

            if let Some(call_event) = parse_carbon_call_event(element, self.account_bare_jid()) {
                return vec![ClientEvent::Call(Box::new(call_event))];
            }
        }

        if let Some(call_event) = messaging::parse_call_event(element) {
            let mut events = Vec::new();
            if let Some(ack) = jingle_iq_set_ack(element) {
                events.push(ClientEvent::Connection(ConnectionEvent::OutboundMessage(
                    TransportMessage::Element(ack),
                )));
            }
            events.push(ClientEvent::Call(Box::new(call_event)));
            return events;
        }

        if let Some(ev) = messaging::parse(element) {
            return vec![ClientEvent::Messaging(ev)];
        }

        vec![ClientEvent::UnhandledStanza(element.clone())]
    }
}

impl XmppRuntime {
    fn account_bare_jid(&self) -> &jid::BareJid {
        match &self.config.auth {
            AuthenticationConfig::OAuthBearer(config) => &config.account,
        }
    }
}

fn parse_carbon_call_event(
    element: &Element,
    account: &jid::BareJid,
) -> Option<crate::messaging::InboundCallEvent> {
    const NS_CARBONS: &str = "urn:xmpp:carbons:2";
    const NS_FORWARD: &str = "urn:xmpp:forward:0";

    if element.name() != "message" {
        return None;
    }
    let outer_from = element.attr("from")?.parse::<jid::Jid>().ok()?.to_bare();
    if &outer_from != account {
        return None;
    }

    for carbon in element.children().filter(|child| {
        child.ns() == NS_CARBONS && (child.name() == "sent" || child.name() == "received")
    }) {
        let Some(forwarded) = carbon.get_child("forwarded", NS_FORWARD) else {
            continue;
        };
        let Some(inner_message) = forwarded
            .children()
            .find(|child| child.name() == "message" && child.ns() == "jabber:client")
        else {
            continue;
        };
        if let Some(call_event) = crate::messaging::parse_call_event(inner_message) {
            return Some(call_event);
        }
    }

    None
}

fn jingle_iq_set_ack(inbound: &Element) -> Option<Element> {
    if inbound.name() != "iq" || inbound.attr("type") != Some("set") {
        return None;
    }
    let id = inbound.attr("id")?;
    let from = inbound.attr("from")?;
    let to = inbound.attr("to");

    let mut builder = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), from);
    if let Some(to) = to {
        builder = builder.attr(minidom::rxml::xml_ncname!("from").to_owned(), to);
    }
    Some(builder.build())
}
