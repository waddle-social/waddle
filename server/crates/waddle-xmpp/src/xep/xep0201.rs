//! XEP-0201: Best Practices for Message Threads
//!
//! The `<thread/>` element is defined by RFC 6121 (no namespace). XEP-0201 is
//! an Informational XEP that standardises how clients and servers should
//! generate, propagate, and optionally nest threads via the `parent` attribute.
//!
//! Wire shape:
//!
//! ```xml
//! <thread parent='root-thread'>child-thread</thread>
//! ```
//!
//! The typed `xmpp_parsers::message::Message::thread` field only carries the
//! thread id; for the optional `parent=` attribute this module exposes helpers
//! that operate on the raw `minidom::Element` form of the message.
//!
//! Waddle advertises `urn:xmpp:threads:0` in disco#info so that clients can
//! discover thread-aware services/rooms.

use minidom::Element;
use xmpp_parsers::message::Message;

pub use crate::xep::xep0461::{set_thread_id, thread_id_from_message};

/// Waddle discovery feature string for XEP-0201 thread support.
pub const NS_THREAD_FEATURE: &str = "urn:xmpp:threads:0";

/// The RFC 6121 thread element name.
pub const THREAD_ELEMENT: &str = "thread";

/// Thread identifier plus an optional parent (XEP-0201 nesting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadInfo {
    /// The thread identifier (element text content).
    pub id: String,
    /// Optional parent thread (XEP-0201 `parent=` attribute).
    pub parent: Option<String>,
}

impl ThreadInfo {
    /// Build a root thread with no parent.
    pub fn root(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent: None,
        }
    }

    /// Build a nested thread with a parent id.
    pub fn child(id: impl Into<String>, parent: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent: Some(parent.into()),
        }
    }
}

fn find_thread_element(msg_xml: &Element) -> Option<&Element> {
    msg_xml
        .children()
        .find(|child| child.name() == THREAD_ELEMENT)
}

/// Parse thread info from a raw message element.
///
/// The `xmpp_parsers::message::Message::thread` field is a plain `String` and
/// drops the `parent` attribute — callers that need `parent` must use this
/// element-level helper.
pub fn parse_thread_info(msg_xml: &Element) -> Option<ThreadInfo> {
    let thread_elem = find_thread_element(msg_xml)?;
    let text = thread_elem.text();
    let id = text.trim();
    if id.is_empty() {
        return None;
    }
    let parent = thread_elem
        .attr("parent")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Some(ThreadInfo {
        id: id.to_owned(),
        parent,
    })
}

/// Read the optional thread parent from a parsed `Message`.
///
/// Round-trips through a `minidom::Element` because the typed field drops the
/// attribute. Returns `None` if there is no `<thread/>` or if `parent=` is
/// absent/blank.
pub fn thread_parent_from_message(msg: &Message) -> Option<String> {
    let xml: Element = Element::from(msg.clone());
    parse_thread_info(&xml).and_then(|info| info.parent)
}

/// Build a `<thread/>` element with optional `parent` attribute.
pub fn build_thread_element(info: &ThreadInfo) -> Element {
    let mut builder = Element::builder(THREAD_ELEMENT, "");
    if let Some(parent) = info.parent.as_deref() {
        builder = builder.attr("parent", parent);
    }
    builder.append(info.id.as_str()).build()
}

/// Replace any `<thread/>` children on a raw message element with one built
/// from `info`. Namespace-agnostic because RFC 6121 `<thread/>` may be emitted
/// in `jabber:client`, `jabber:server`, or the empty namespace depending on
/// the serializer.
pub fn install_thread_element(msg_xml: &mut Element, info: &ThreadInfo) {
    let to_remove: Vec<(String, String)> = msg_xml
        .children()
        .filter(|c| c.name() == THREAD_ELEMENT)
        .map(|c| (c.name().to_owned(), c.ns()))
        .collect();
    for (name, ns) in to_remove {
        msg_xml.remove_child(&name, ns.as_str());
    }
    msg_xml.append_child(build_thread_element(info));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_thread() {
        let xml = "<message xmlns='jabber:client'><thread>root-1</thread></message>"
            .parse::<Element>()
            .expect("valid xml");
        let info = parse_thread_info(&xml).expect("thread");
        assert_eq!(info.id, "root-1");
        assert_eq!(info.parent, None);
    }

    #[test]
    fn parses_child_thread() {
        let xml =
            "<message xmlns='jabber:client'><thread parent='root-1'>child-2</thread></message>"
                .parse::<Element>()
                .expect("valid xml");
        let info = parse_thread_info(&xml).expect("thread");
        assert_eq!(info.id, "child-2");
        assert_eq!(info.parent.as_deref(), Some("root-1"));
    }

    #[test]
    fn empty_thread_returns_none() {
        let xml = "<message xmlns='jabber:client'><thread></thread></message>"
            .parse::<Element>()
            .expect("valid xml");
        assert_eq!(parse_thread_info(&xml), None);
    }

    #[test]
    fn missing_thread_returns_none() {
        let xml = "<message xmlns='jabber:client'/>"
            .parse::<Element>()
            .expect("valid xml");
        assert_eq!(parse_thread_info(&xml), None);
    }

    #[test]
    fn build_element_with_parent() {
        let info = ThreadInfo::child("child-a", "root-a");
        let elem = build_thread_element(&info);
        assert_eq!(elem.name(), THREAD_ELEMENT);
        assert_eq!(elem.attr("parent"), Some("root-a"));
        assert_eq!(elem.text(), "child-a");
    }

    #[test]
    fn build_element_without_parent() {
        let info = ThreadInfo::root("root-a");
        let elem = build_thread_element(&info);
        assert_eq!(elem.attr("parent"), None);
        assert_eq!(elem.text(), "root-a");
    }

    #[test]
    fn set_thread_id_on_root_message() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_id(&mut msg, "abc");
        assert_eq!(thread_id_from_message(&msg).as_deref(), Some("abc"));
        assert_eq!(thread_parent_from_message(&msg), None);
    }

    #[test]
    fn install_thread_element_strips_existing() {
        let mut xml = "<message xmlns='jabber:client'><thread>old</thread></message>"
            .parse::<Element>()
            .expect("valid xml");
        install_thread_element(&mut xml, &ThreadInfo::child("new", "root"));
        let count = xml
            .children()
            .filter(|c| c.name() == THREAD_ELEMENT)
            .count();
        assert_eq!(count, 1);
        let info = parse_thread_info(&xml).expect("thread after install");
        assert_eq!(info.id, "new");
        assert_eq!(info.parent.as_deref(), Some("root"));
    }

    #[test]
    fn thread_feature_constant() {
        assert_eq!(NS_THREAD_FEATURE, "urn:xmpp:threads:0");
    }
}
