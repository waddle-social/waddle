//! Shared recursive IQ error-echo payload sanitization for sensitive credential
//! namespaces (`urn:waddle:credential:*`, `urn:waddle:inference:credential:*`,
//! and `urn:waddle:transports:livekit:0`).
//!
//! Ensures that no IQ error bounce path can echo top-level or deeply nested
//! credential payloads back over the wire or into XEP-0198 `sm_unacked`.

use minidom::{Element, Node};

use crate::protocol::handlers::jingle::credential_free_jingle_echo;
use crate::xep::xep0166::NS_JINGLE;

const NS_WADDLE_LIVEKIT_TRANSPORT: &str = "urn:waddle:transports:livekit:0";

/// Returns `true` if `ns` is a sensitive credential namespace that MUST NEVER
/// be echoed in IQ error bounces or persisted in `sm_unacked`.
///
/// Non-credential `urn:waddle:*` namespaces (e.g. `urn:waddle:pin:0`,
/// `urn:waddle:dnd:0`, `urn:waddle:inbox:0`, `urn:waddle:extension:1:*`) return
/// `false` so existing XMPP features remain unaffected.
#[must_use]
pub fn is_sensitive_credential_namespace(ns: &str) -> bool {
    ns == "urn:waddle:credential"
        || ns.starts_with("urn:waddle:credential:")
        || ns.starts_with("urn:waddle:inference:credential:")
        || ns == NS_WADDLE_LIVEKIT_TRANSPORT
}

/// Sanitizes an IQ request payload before echoing it inside an `<iq type='error'>`
/// bounce reply.
///
/// - Returns `None` if the top-level `payload` element itself belongs to a
///   sensitive credential namespace.
/// - Otherwise applies `credential_free_jingle_echo` when `payload` is a
///   `<jingle xmlns='urn:xmpp:jingle:1'/>` element, and recursively strips all
///   descendant elements at any depth whose namespace matches
///   [`is_sensitive_credential_namespace`].
#[must_use]
pub fn sanitize_iq_error_echo_payload(payload: &Element) -> Option<Element> {
    if is_sensitive_credential_namespace(payload.ns().as_str()) {
        return None;
    }
    let base = if payload.is("jingle", NS_JINGLE) {
        credential_free_jingle_echo(payload)
    } else {
        payload.clone()
    };
    Some(strip_sensitive_descendants(&base))
}

/// Recursively rebuilds `el`, preserving its name, namespace, attributes, and
/// non-sensitive text/child nodes while dropping every descendant element whose
/// namespace matches [`is_sensitive_credential_namespace`].
#[must_use]
pub fn strip_sensitive_descendants(el: &Element) -> Element {
    let mut builder = Element::builder(el.name(), el.ns());
    for ((attr_ns, attr_name), attr_val) in el.attrs() {
        builder = builder.attr_ns(attr_ns.clone(), attr_name.to_owned(), attr_val);
    }
    for node in el.nodes() {
        match node {
            Node::Text(text) => {
                builder = builder.append(text.as_str());
            }
            Node::Element(child) => {
                if !is_sensitive_credential_namespace(child.ns().as_str()) {
                    builder = builder.append(strip_sensitive_descendants(child));
                }
            }
        }
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_top_level_and_deeply_nested_credential_elements() {
        let top = Element::builder("enroll-credential", "urn:waddle:credential:1")
            .append("sk-secret-top")
            .build();
        assert!(sanitize_iq_error_echo_payload(&top).is_none());

        let nested = Element::builder("query", "http://jabber.org/protocol/muc#admin")
            .append(
                Element::builder("wrapper", "urn:test")
                    .append(
                        Element::builder("enroll-credential", "urn:waddle:credential:1")
                            .append("sk-secret-nested")
                            .build(),
                    )
                    .append(Element::builder("keep", "urn:test").append("ok").build())
                    .build(),
            )
            .build();
        let sanitized = sanitize_iq_error_echo_payload(&nested).expect("outer preserved");
        let xml = String::from(&sanitized);
        assert!(!xml.contains("sk-secret-nested"));
        assert!(!xml.contains("urn:waddle:credential:1"));
        assert!(xml.contains("ok"));
    }

    #[test]
    fn preserves_non_credential_waddle_namespaces() {
        let pin = Element::builder("pin", "urn:waddle:pin:0").build();
        assert!(!is_sensitive_credential_namespace("urn:waddle:pin:0"));
        assert_eq!(sanitize_iq_error_echo_payload(&pin), Some(pin));
    }

    #[test]
    fn preserves_namespaced_attributes_while_stripping_sensitive_children() {
        let raw_xml = "<query xmlns='http://jabber.org/protocol/muc#admin' \
            xml:lang='en' plain='yes' xmlns:ex='urn:example:attr' ex:tag='v1'>\
            <enroll-credential xmlns='urn:waddle:credential:1'>sk-secret-attr</enroll-credential>\
            <item role='member' xml:lang='fr'>kept</item>\
            </query>";
        let parsed: Element = raw_xml.parse().expect("valid xml");
        let sanitized = sanitize_iq_error_echo_payload(&parsed).expect("outer preserved");

        let xml_ns = minidom::rxml::Namespace::XML;
        let ex_ns = minidom::rxml::Namespace::from("urn:example:attr");
        assert_eq!(sanitized.attr_ns(&xml_ns, "lang"), Some("en"));
        assert_eq!(sanitized.attr("lang"), None);
        assert_eq!(sanitized.attr("plain"), Some("yes"));
        assert_eq!(sanitized.attr_ns(&ex_ns, "tag"), Some("v1"));

        let item = sanitized
            .get_child("item", "http://jabber.org/protocol/muc#admin")
            .expect("item child preserved");
        assert_eq!(item.attr_ns(&xml_ns, "lang"), Some("fr"));
        assert_eq!(item.attr("role"), Some("member"));

        let serialized = String::from(&sanitized);
        assert!(
            serialized.contains("xml:lang='en'") || serialized.contains("xml:lang=\"en\""),
            "expected xml:lang='en' in serialized output: {serialized}"
        );
        assert!(!serialized.contains("sk-secret-attr"));
        assert!(!serialized.contains("urn:waddle:credential:1"));
    }
}
