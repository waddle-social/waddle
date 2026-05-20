//! XEP-0201 Best Practices for Message Threads.
//!
//! Threads are just `<thread/>` children on messages. A thread may optionally
//! reference a parent thread via the `parent` attribute, which is what
//! Waddle uses to model nested thread conversations.

use minidom::Element;

/// Client namespace — `<thread/>` lives in the same namespace as the parent
/// `<message/>` (i.e. `jabber:client`), but we allow empty ns too since
/// serialized elements sometimes collapse it.
pub const NS_CLIENT: &str = "jabber:client";

/// A typed thread reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRef {
    /// Thread identifier (text content of `<thread/>`).
    pub id: String,
    /// Optional parent thread id for nested threads (the `parent` attribute).
    pub parent: Option<String>,
}

/// Build a `<thread/>` element, optionally with a `parent` attribute for
/// nested threads.
pub fn build_thread_element(thread: &ThreadRef) -> Element {
    let mut builder = Element::builder("thread", NS_CLIENT);
    if let Some(parent) = thread.parent.as_deref() {
        builder = builder.attr(minidom::rxml::xml_ncname!("parent").to_owned(), parent);
    }
    builder.append(thread.id.as_str()).build()
}

/// Parse the `<thread/>` child of a message, if any. Looks first in the
/// client namespace and then in the empty namespace to handle both WebSocket
/// and c2s framings.
pub fn parse_thread(message: &Element) -> Option<ThreadRef> {
    let thread_el = message
        .get_child("thread", NS_CLIENT)
        .or_else(|| message.get_child("thread", ""))?;

    let id = thread_el.text();
    if id.is_empty() {
        return None;
    }
    let parent = thread_el.attr("parent").map(str::to_string);
    Some(ThreadRef { id, parent })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_el(xml: &str) -> Element {
        xml.parse().expect("invalid XML")
    }

    #[test]
    fn parses_thread_without_parent() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <thread>abc-123</thread>\
             </message>",
        );
        let t = parse_thread(&el).expect("expected thread");
        assert_eq!(t.id, "abc-123");
        assert!(t.parent.is_none());
    }

    #[test]
    fn parses_thread_with_parent() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <thread parent='root-thread'>child-thread</thread>\
             </message>",
        );
        let t = parse_thread(&el).expect("expected thread");
        assert_eq!(t.id, "child-thread");
        assert_eq!(t.parent.as_deref(), Some("root-thread"));
    }

    #[test]
    fn returns_none_when_no_thread_child() {
        let el = parse_el("<message xmlns='jabber:client'><body>hi</body></message>");
        assert!(parse_thread(&el).is_none());
    }

    #[test]
    fn returns_none_for_empty_thread() {
        let el = parse_el(
            "<message xmlns='jabber:client'>\
               <thread></thread>\
             </message>",
        );
        assert!(parse_thread(&el).is_none());
    }

    #[test]
    fn builds_thread_element_without_parent() {
        let t = ThreadRef {
            id: "abc-123".to_string(),
            parent: None,
        };
        let el = build_thread_element(&t);
        assert_eq!(el.name(), "thread");
        assert_eq!(el.ns(), NS_CLIENT);
        assert!(el.attr("parent").is_none());
        assert_eq!(el.text(), "abc-123");
    }

    #[test]
    fn builds_thread_element_with_parent() {
        let t = ThreadRef {
            id: "child".to_string(),
            parent: Some("root".to_string()),
        };
        let el = build_thread_element(&t);
        assert_eq!(el.attr("parent"), Some("root"));
        assert_eq!(el.text(), "child");
    }

    #[test]
    fn build_and_parse_thread_roundtrip() {
        let t = ThreadRef {
            id: "thread-xyz".to_string(),
            parent: Some("parent-abc".to_string()),
        };
        let message = Element::builder("message", NS_CLIENT)
            .append(build_thread_element(&t))
            .build();
        let parsed = parse_thread(&message).expect("parses");
        assert_eq!(parsed, t);
    }
}
