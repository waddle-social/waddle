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

/// XEP-0060 owner namespace.
pub const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";

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

/// A `set` IQ without a `to`, which the server handles on behalf of the
/// sender's own account (RFC 6120 §10.3.3); a PEP request to the
/// account's own node service.
pub fn own_set(id: &str, payload: Element) -> Iq {
    Iq::Set {
        from: None,
        to: None,
        id: id.to_owned(),
        payload,
    }
}

/// A `get` IQ without a `to`, which the server handles on behalf of the
/// sender's own account (RFC 6120 §10.3.3).
pub fn own_get(id: &str, payload: Element) -> Iq {
    Iq::Get {
        from: None,
        to: None,
        id: id.to_owned(),
        payload,
    }
}

/// One RFC 7395 frame parsed as the XML element it carries, or `None`
/// for a frame that is not one.
pub fn parse_frame(frame: &str) -> Option<Element> {
    frame.parse::<Element>().ok()
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

/// A XEP-0060 §8.9 owner affiliations request for `node`: a get when
/// `changes` is empty, otherwise a set of each `(jid, affiliation)`.
pub fn pubsub_owner_affiliations(node: &str, changes: &[(&str, &str)]) -> Element {
    Element::builder("pubsub", NS_PUBSUB_OWNER)
        .append(
            Element::builder("affiliations", NS_PUBSUB_OWNER)
                .attr(attribute("node"), node)
                .append_all(changes.iter().map(|(jid, affiliation)| {
                    Element::builder("affiliation", NS_PUBSUB_OWNER)
                        .attr(attribute("jid"), *jid)
                        .attr(attribute("affiliation"), *affiliation)
                        .build()
                }))
                .build(),
        )
        .build()
}

/// The `(jid, affiliation)` pairs of the owner `<affiliations/>` element
/// for `node` inside `payload`, in document order.
pub fn owner_affiliations_of(payload: &Element, node: &str) -> Option<Vec<(String, String)>> {
    let affiliations = payload
        .get_child("affiliations", NS_PUBSUB_OWNER)
        .filter(|affiliations| affiliations.attr("node") == Some(node))?;
    affiliations
        .children()
        .filter(|child| child.is("affiliation", NS_PUBSUB_OWNER))
        .map(|child| {
            Some((
                child.attr("jid")?.to_owned(),
                child.attr("affiliation")?.to_owned(),
            ))
        })
        .collect()
}
