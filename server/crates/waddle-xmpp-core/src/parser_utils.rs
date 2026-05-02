//! Parser utility functions shared across modules.
//!
//! This module provides common parser utilities that are used by both
//! the stream parser and message serialization logic to avoid code duplication.

use minidom::Element;
use xmpp_parsers::message::Message;

/// Ensures `<thread/>` element is present in a Message Element.
///
/// xmpp_parsers 0.21 drops `<thread/>` when serializing Message back to Element.
/// This function re-attaches it so RFC 6121 / XEP-0201 metadata survives.
///
/// ## Arguments
/// - `element`: The message Element to potentially modify
/// - `thread_id`: The thread ID from the parsed Message
pub fn ensure_thread_element(element: &mut Element, thread_id: Option<&str>) {
    if let Some(id) = thread_id {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            // skip empty thread
        } else {
            let stanza_ns = element.ns();
            if element
                .children()
                .any(|child| crate::xep0201::is_thread_element_for_stanza(child, &stanza_ns))
            {
                return;
            }
            let info = crate::xep0201::ThreadInfo {
                id: trimmed.to_owned(),
                parent: None,
            };
            element.append_child(crate::xep0201::build_thread_element(&info, &stanza_ns));
        }
    }
}

/// Extract thread parent attribute from a message Element before parsing.
///
/// xmpp_parsers 0.21 `Thread(String)` only carries the thread id — the XEP-0201
/// `parent` attribute would be lost. This function extracts it before parsing.
///
/// ## Arguments
/// - `element`: The message Element to inspect
///
/// ## Returns
/// The parent attribute value if present and non-empty, None otherwise.
pub fn extract_thread_parent(element: &Element) -> Option<String> {
    let stanza_ns = element.ns();
    element
        .children()
        .find(|child| crate::xep0201::is_thread_element_for_stanza(child, &stanza_ns))
        .and_then(|child| {
            child
                .attr("parent")
                .map(str::trim)
                .filter(|parent| !parent.is_empty())
                .map(str::to_owned)
        })
}

/// Re-attach thread parent attribute to a Message payload.
///
/// After extracting the parent attribute with `extract_thread_parent`, this
/// function re-attaches it as a raw payload Element so the attribute survives
/// the round-trip. Builds the element via the canonical
/// [`crate::xep0201::build_thread_element`] so the wire shape is defined in
/// exactly one place.
///
/// ## Arguments
/// - `msg`: The parsed Message to modify
/// - `thread_parent`: The parent attribute value
/// - `stanza_ns`: The namespace for the thread element
pub fn reattach_thread_parent(msg: &mut Message, thread_parent: String, stanza_ns: &str) {
    if let Some(thread) = msg.thread.take() {
        let info = crate::xep0201::ThreadInfo {
            id: thread.0,
            parent: Some(thread_parent),
        };
        msg.payloads
            .push(crate::xep0201::build_thread_element(&info, stanza_ns));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_thread_element_adds_thread() {
        let mut element = Element::builder("message", "jabber:client")
            .attr("from", "alice@localhost")
            .build();

        ensure_thread_element(&mut element, Some("thread-123"));

        let thread = element.get_child("thread", "jabber:client").unwrap();
        assert_eq!(thread.text(), "thread-123");
    }

    #[test]
    fn test_ensure_thread_element_skips_empty() {
        let mut element = Element::builder("message", "jabber:client").build();

        ensure_thread_element(&mut element, Some("  "));

        assert!(element.get_child("thread", "jabber:client").is_none());
    }

    #[test]
    fn test_ensure_thread_element_skips_existing() {
        let mut element = Element::builder("message", "jabber:client")
            .append(
                Element::builder("thread", "jabber:client")
                    .append("existing-thread")
                    .build(),
            )
            .build();

        ensure_thread_element(&mut element, Some("new-thread"));

        let thread = element.get_child("thread", "jabber:client").unwrap();
        assert_eq!(thread.text(), "existing-thread");
    }

    #[test]
    fn test_ensure_thread_element_ignores_unrelated_namespaced_thread() {
        let mut element = Element::builder("message", "jabber:client")
            .append(
                Element::builder("thread", "urn:example:other:0")
                    .attr("kind", "extension")
                    .append("not-xep-0201")
                    .build(),
            )
            .build();

        ensure_thread_element(&mut element, Some("thread-123"));

        assert_eq!(
            element.get_child("thread", "jabber:client").unwrap().text(),
            "thread-123"
        );
        assert!(element
            .children()
            .any(|child| child.name() == "thread" && child.ns() == "urn:example:other:0"));
    }

    #[test]
    fn test_ensure_thread_element_ignores_explicit_empty_namespace_thread() {
        let mut element = Element::builder("message", "jabber:client")
            .append(
                Element::builder("thread", "")
                    .attr("kind", "extension")
                    .append("not-xep-0201")
                    .build(),
            )
            .build();

        ensure_thread_element(&mut element, Some("thread-123"));

        assert_eq!(
            element.get_child("thread", "jabber:client").unwrap().text(),
            "thread-123"
        );
        assert!(element
            .children()
            .any(|child| child.name() == "thread" && child.ns().is_empty()));
    }

    #[test]
    fn test_extract_thread_parent() {
        let element = Element::builder("message", "jabber:client")
            .append(
                Element::builder("thread", "jabber:client")
                    .attr("parent", "parent-123")
                    .append("thread-456")
                    .build(),
            )
            .build();

        let parent = extract_thread_parent(&element);
        assert_eq!(parent, Some("parent-123".to_string()));
    }

    #[test]
    fn test_extract_thread_parent_none() {
        let element = Element::builder("message", "jabber:client")
            .append(
                Element::builder("thread", "jabber:client")
                    .append("thread-456")
                    .build(),
            )
            .build();

        let parent = extract_thread_parent(&element);
        assert_eq!(parent, None);
    }

    #[test]
    fn test_extract_thread_parent_ignores_explicit_empty_namespace_thread() {
        let element = Element::builder("message", "jabber:client")
            .append(
                Element::builder("thread", "")
                    .attr("parent", "not-a-stanza-parent")
                    .append("thread-456")
                    .build(),
            )
            .build();

        let parent = extract_thread_parent(&element);
        assert_eq!(parent, None);
    }
}
