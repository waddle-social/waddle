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

            // XEP-0280: unwrap a normal (non-call) carbon copy so the
            // inner message flows through the regular messaging pipeline
            // tagged with its direction (#1243). The §11 forgery rule is
            // applied first: any carbon-shaped stanza whose wrapping
            // 'from' is not our own bare JID MUST be ignored outright —
            // it must not fall through to `messaging::parse`, where the
            // body-less outer envelope would masquerade as a message.
            match unwrap_carbon(element, self.account_bare_jid()) {
                CarbonOutcome::Unwrapped {
                    direction,
                    inner,
                    forwarded_timestamp,
                } => {
                    if let Some(crate::messaging::MessagingEvent::Message(mut message)) =
                        messaging::parse(&inner)
                    {
                        message.carbon = Some(direction);
                        // XEP-0297 §5: prefer the inner element's own
                        // <delay/>, fall back to the forwarded wrapper's.
                        message.timestamp = message.timestamp.or(forwarded_timestamp);
                        return vec![ClientEvent::Messaging(
                            crate::messaging::MessagingEvent::Message(message),
                        )];
                    }
                    // Carbon wrapped something that is not a parseable
                    // message: nothing to surface.
                    return Vec::new();
                }
                CarbonOutcome::Forged => return Vec::new(),
                CarbonOutcome::NotCarbon => {}
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
            let pubsub_events = pubsub_notification_events(&ev);
            let mut events = vec![ClientEvent::Messaging(ev)];
            events.extend(pubsub_events);
            return events;
        }

        vec![ClientEvent::UnhandledStanza(element.clone())]
    }
}

/// Flatten the XEP-0060 notifications attached to a parsed message
/// into standalone [`ClientEvent`]s: §7.2.2.1 item retractions and
/// XEP-0470 attachment-summary updates. Emitted *alongside* the
/// [`ClientEvent::Messaging`] event (which still carries the full
/// `pubsub_events` list) so consumers that only care about these two
/// state transitions need not unpack the message envelope.
///
/// Only the direct message path feeds this: carbon-unwrapped messages
/// deliberately skip it because XEP-0280 carbons only cover
/// chat-eligible messages — a pubsub service's headline notifications
/// are never carbon-copied.
fn pubsub_notification_events(event: &crate::messaging::MessagingEvent) -> Vec<ClientEvent> {
    let crate::messaging::MessagingEvent::Message(message) = event else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for pubsub_event in &message.pubsub_events {
        if let Some(retracted) = pubsub_event.retracted_items() {
            events.push(ClientEvent::PubsubItemsRetracted(retracted));
        }
        for summary in pubsub_event.attachment_summaries() {
            events.push(ClientEvent::PubsubAttachmentSummary(summary));
        }
    }
    events
}

impl XmppRuntime {
    fn account_bare_jid(&self) -> &jid::BareJid {
        match &self.config.auth {
            AuthenticationConfig::OAuthBearer(config) => &config.account,
        }
    }
}

/// XEP-0280 carbons namespace (`<sent/>` / `<received/>` envelopes).
const NS_CARBONS: &str = "urn:xmpp:carbons:2";
/// XEP-0297 stanza forwarding namespace (`<forwarded/>`).
const NS_FORWARD: &str = "urn:xmpp:forward:0";
/// XEP-0203 delayed delivery namespace (forwarded-level `<delay/>`).
const NS_DELAY: &str = "urn:xmpp:delay";

/// Result of probing a `<message>` for a XEP-0280 carbon envelope.
enum CarbonOutcome {
    /// A verified carbon: the wrapping stanza came from the account's
    /// own bare JID and contained `<sent|received>/<forwarded>/<message>`.
    Unwrapped {
        direction: crate::messaging::CarbonDirection,
        inner: Element,
        /// XEP-0297 §5: the `<forwarded/>` wrapper may carry the original
        /// delivery time as a `<delay/>` child. Propagated so the inner
        /// message keeps its authoritative timestamp when the inner
        /// element itself has none.
        forwarded_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// Carbon-shaped but the wrapping 'from' is not our bare JID —
    /// XEP-0280 §11 says such copies MUST be ignored.
    Forged,
    /// Not a carbon envelope at all.
    NotCarbon,
}

/// XEP-0280 §6.1/§6.2 + §11: the wrapping message's `from` MUST be the
/// Carbons-enabled user's **bare** JID — a full JID (even one of the
/// account's own resources) is not a server-generated carbon envelope
/// and MUST be ignored. A missing `from` is also rejected: every
/// XEP-0280 envelope shape stamps it, and accepting an absent `from`
/// would widen the trust boundary for no conformant sender.
fn carbon_envelope_from_own_bare(element: &Element, account: &jid::BareJid) -> bool {
    element
        .attr("from")
        .and_then(|from| from.parse::<jid::Jid>().ok())
        .is_some_and(|from| from.resource().is_none() && &from.to_bare() == account)
}

/// Probe `element` for a XEP-0280 `<sent/>`/`<received/>` carbon and
/// extract the forwarded inner `<message>` after the §11 own-bare-JID
/// forgery check.
fn unwrap_carbon(element: &Element, account: &jid::BareJid) -> CarbonOutcome {
    let Some(carbon) = element.children().find(|child| {
        child.ns() == NS_CARBONS && (child.name() == "sent" || child.name() == "received")
    }) else {
        return CarbonOutcome::NotCarbon;
    };

    if !carbon_envelope_from_own_bare(element, account) {
        return CarbonOutcome::Forged;
    }

    let direction = if carbon.name() == "sent" {
        crate::messaging::CarbonDirection::Sent
    } else {
        crate::messaging::CarbonDirection::Received
    };
    let Some(forwarded) = carbon.get_child("forwarded", NS_FORWARD) else {
        return CarbonOutcome::Forged;
    };
    let Some(inner) = forwarded
        .children()
        .find(|child| child.name() == "message" && child.ns() == "jabber:client")
    else {
        return CarbonOutcome::Forged;
    };
    let forwarded_timestamp = forwarded
        .get_child("delay", NS_DELAY)
        .and_then(|delay| delay.attr("stamp"))
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .map(|stamp| stamp.with_timezone(&chrono::Utc));

    CarbonOutcome::Unwrapped {
        direction,
        inner: inner.clone(),
        forwarded_timestamp,
    }
}

fn parse_carbon_call_event(
    element: &Element,
    account: &jid::BareJid,
) -> Option<crate::messaging::InboundCallEvent> {
    if element.name() != "message" {
        return None;
    }
    if !carbon_envelope_from_own_bare(element, account) {
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
