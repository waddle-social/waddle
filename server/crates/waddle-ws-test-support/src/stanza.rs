//! Typed IQ requests and XEP-0060 payloads that suites send to an
//! in-process server, and the reply shapes they assert on.
//!
//! The builders return `xmpp_parsers` values and minidom elements; a suite
//! adds the payloads of its own protocol.

use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::rxml::NcName;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::DefinedCondition;

/// XEP-0060 namespace.
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// An XML attribute name the builders below use.
fn attribute(name: &str) -> NcName {
    NcName::try_from(name).expect("valid XML attribute name")
}

/// A `set` IQ to `to` carrying `payload`.
pub fn set(id: &str, to: &str, payload: Element) -> Iq {
    Iq::Set {
        from: None,
        to: Some(to.parse().expect("IQ destination")),
        id: id.to_owned(),
        payload,
    }
}

/// A `get` IQ to `to` carrying `payload`.
pub fn get(id: &str, to: &str, payload: Element) -> Iq {
    Iq::Get {
        from: None,
        to: Some(to.parse().expect("IQ destination")),
        id: id.to_owned(),
        payload,
    }
}

/// Whether an IQ reply is a result.
pub fn is_result(reply: &Iq) -> bool {
    matches!(reply, Iq::Result { .. })
}

/// Whether an IQ reply is an error with `condition`.
pub fn is_error(reply: &Iq, condition: DefinedCondition) -> bool {
    matches!(reply, Iq::Error { error, .. } if error.defined_condition == condition)
}

/// The payload of a result IQ, if it is one and carries any.
pub fn result_payload(reply: &Iq) -> Option<&Element> {
    match reply {
        Iq::Result {
            payload: Some(payload),
            ..
        } => Some(payload),
        _ => None,
    }
}

/// A XEP-0060 publish of one item to `node`.
pub fn pubsub_publish(node: &str, item_id: &str, payload: Element) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("publish", NS_PUBSUB)
                .attr(attribute("node"), node)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(attribute("id"), item_id)
                        .append(payload)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// A XEP-0060 retraction of one item from `node`.
pub fn pubsub_retract(node: &str, item_id: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("retract", NS_PUBSUB)
                .attr(attribute("node"), node)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(attribute("id"), item_id)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// A XEP-0060 subscription of `jid` to `node`.
pub fn pubsub_subscribe(node: &str, jid: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("subscribe", NS_PUBSUB)
                .attr(attribute("node"), node)
                .attr(attribute("jid"), jid)
                .build(),
        )
        .build()
}

/// A XEP-0060 items request for `node`.
pub fn pubsub_items(node: &str) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("items", NS_PUBSUB)
                .attr(attribute("node"), node)
                .build(),
        )
        .build()
}
