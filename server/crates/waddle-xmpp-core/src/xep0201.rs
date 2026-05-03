//! XEP-0201: Best Practices for Message Threads
//!
//! The `<thread/>` element is defined by RFC 6121 as a child of the message
//! stanza. XEP-0201 is an Informational XEP that standardises how clients and
//! servers should generate, propagate, and optionally nest threads via the
//! `parent` attribute.
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

use crate::mam::ThreadId;
use minidom::Element;
use serde::{Deserialize, Serialize};
use xmpp_parsers::message::{Message, Thread};

/// Waddle discovery feature string for XEP-0201 thread support.
pub const NS_THREAD_FEATURE: &str = "urn:xmpp:threads:0";

/// The RFC 6121 thread element name.
pub const THREAD_ELEMENT: &str = "thread";

/// Client-to-server stanza namespace for message payloads.
pub const CLIENT_STANZA_NS: &str = "jabber:client";

/// Server-to-server stanza namespace for message payloads.
pub const SERVER_STANZA_NS: &str = "jabber:server";

/// Return true for the RFC 6121 `<thread/>` child belonging to `stanza_ns`.
///
/// Same-local-name extension payloads in another namespace are not message
/// thread metadata and must be preserved.
pub fn is_thread_element_for_stanza(element: &Element, stanza_ns: &str) -> bool {
    element.name() == THREAD_ELEMENT && element.ns() == stanza_ns
}

fn is_message_thread_payload_for_stanza(element: &Element, stanza_ns: &str) -> bool {
    element.name() == THREAD_ELEMENT && element.ns() == stanza_ns
}

/// Thread identifier plus an optional parent (XEP-0201 nesting).
///
/// Derives `Serialize`/`Deserialize` so it can flow through any
/// container that owns it (notably `mam::ArchivedMessage.thread`,
/// which is serialized as part of the rich-payload JSON snapshot).
/// The on-the-wire shape is `<thread parent='X'>id</thread>`; the
/// JSON shape is `{ "id": "...", "parent": "..." | null }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadInfo {
    /// The thread identifier (element text content).
    pub id: ThreadId,
    /// Optional parent thread (XEP-0201 `parent=` attribute).
    pub parent: Option<ThreadId>,
}

impl ThreadInfo {
    /// Build a root thread with no parent.
    pub fn root(id: ThreadId) -> Self {
        Self { id, parent: None }
    }

    /// Build a nested thread with a parent id.
    pub fn child(id: ThreadId, parent: ThreadId) -> Self {
        Self {
            id,
            parent: Some(parent),
        }
    }
}

fn find_thread_element(msg_xml: &Element) -> Option<&Element> {
    let stanza_ns = msg_xml.ns();
    msg_xml
        .children()
        .find(|child| is_thread_element_for_stanza(child, &stanza_ns))
}

/// Parse thread info from a raw message element.
///
/// The `xmpp_parsers::message::Message::thread` field is a plain `String` and
/// drops the `parent` attribute — callers that need `parent` must use this
/// element-level helper.
pub fn parse_thread_info(msg_xml: &Element) -> Option<ThreadInfo> {
    let thread_elem = find_thread_element(msg_xml)?;
    let id = ThreadId::new(thread_elem.text().trim().to_owned())?;
    let parent = thread_elem
        .attr("parent")
        .map(str::trim)
        .and_then(ThreadId::new);
    Some(ThreadInfo { id, parent })
}

/// Read XEP-0201 thread info from a parsed `Message` (id and optional parent).
///
/// `xmpp_parsers::Message::thread` is a typed `Option<Thread(String)>` and
/// silently drops the XEP-0201 `parent` attribute at parse time. Waddle works
/// around this with [`super::parser_utils::reattach_thread_parent`], which
/// moves the `<thread parent='X'>id</thread>` element into `msg.payloads`
/// (and clears `msg.thread`) at the inbound parse boundary so the parent
/// survives the rest of the pipeline.
///
/// This helper preserves that invariant: it reads the payload form first,
/// returning `ThreadInfo { id, parent }` if a `<thread/>` element with
/// non-empty body is present, then falls back to the typed `msg.thread`
/// field (parent unrecoverable) when no payload thread exists. Callers that
/// need parent on archive write should ensure `reattach_thread_parent` runs
/// upstream of this call.
pub fn thread_info_from_message_in_stanza_ns(
    msg: &Message,
    stanza_ns: impl AsRef<str>,
) -> Option<ThreadInfo> {
    let stanza_ns = stanza_ns.as_ref();
    if let Some(thread_elem) = msg
        .payloads
        .iter()
        .find(|elem| is_message_thread_payload_for_stanza(elem, stanza_ns))
    {
        let id = ThreadId::new(thread_elem.text().trim().to_owned())?;
        let parent = thread_elem
            .attr("parent")
            .map(str::trim)
            .and_then(ThreadId::new);
        return Some(ThreadInfo { id, parent });
    }
    msg.thread
        .as_ref()
        .and_then(|thread| ThreadId::new(thread.0.clone()))
        .map(ThreadInfo::root)
}

/// Read XEP-0201 thread info from a client-stanza parsed `Message`.
pub fn thread_info_from_message(msg: &Message) -> Option<ThreadInfo> {
    thread_info_from_message_in_stanza_ns(msg, CLIENT_STANZA_NS)
}

/// Build a `<thread/>` element with optional `parent` attribute in the given
/// stanza namespace.
///
/// RFC 6121 scopes `<thread/>` to the enclosing message's namespace. Pass the
/// parent message's namespace (typically `jabber:client` or `jabber:server`)
/// so the serializer does not emit a spurious `xmlns=""`.
pub fn build_thread_element(info: &ThreadInfo, ns: impl AsRef<str>) -> Element {
    let mut builder = Element::builder(THREAD_ELEMENT, ns.as_ref());
    if let Some(parent) = info.parent.as_ref().map(ThreadId::as_str) {
        builder = builder.attr("parent", parent);
    }
    builder.append(info.id.as_str()).build()
}

/// Replace any `<thread/>` children on a raw message element with one built
/// from `info`, inheriting the message's namespace.
pub fn install_thread_element(msg_xml: &mut Element, info: &ThreadInfo) {
    let stanza_ns = msg_xml.ns().to_string();
    let to_remove: Vec<(String, String)> = msg_xml
        .children()
        .filter(|c| is_thread_element_for_stanza(c, &stanza_ns))
        .map(|c| (c.name().to_owned(), c.ns()))
        .collect();
    for (name, ns) in to_remove {
        msg_xml.remove_child(&name, ns.as_str());
    }
    msg_xml.append_child(build_thread_element(info, &stanza_ns));
}

/// Read RFC 6121 `<thread/>` identifier from a message.
///
/// Checks both the typed `Message::thread` field and any raw `<thread/>`
/// payload element — the latter is used to preserve compatibility with
/// callers that constructed messages by appending an element rather than
/// setting the typed field.
pub fn thread_id_from_message(msg: &Message) -> Option<String> {
    thread_id_from_message_in_stanza_ns(msg, CLIENT_STANZA_NS)
}

/// Read RFC 6121 `<thread/>` identifier from a message in `stanza_ns`.
pub fn thread_id_from_message_in_stanza_ns(
    msg: &Message,
    stanza_ns: impl AsRef<str>,
) -> Option<String> {
    if let Some(thread) = msg.thread.as_ref() {
        return Some(thread.0.clone());
    }
    let stanza_ns = stanza_ns.as_ref();
    msg.payloads
        .iter()
        .find(|elem| is_message_thread_payload_for_stanza(elem, stanza_ns))
        .map(|elem| elem.text())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// Set RFC 6121 `<thread/>` identifier on a message.
pub fn set_thread_id(msg: &mut Message, thread_id: impl Into<String>) {
    msg.thread = Some(Thread(thread_id.into()));
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
        assert_eq!(info.id.as_str(), "root-1");
        assert_eq!(info.parent, None);
    }

    #[test]
    fn parses_child_thread() {
        let xml =
            "<message xmlns='jabber:client'><thread parent='root-1'>child-2</thread></message>"
                .parse::<Element>()
                .expect("valid xml");
        let info = parse_thread_info(&xml).expect("thread");
        assert_eq!(info.id.as_str(), "child-2");
        assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root-1"));
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
    fn explicit_empty_namespace_thread_is_not_stanza_thread() {
        let xml = Element::builder("message", "jabber:client")
            .append(
                Element::builder(THREAD_ELEMENT, "")
                    .attr("parent", "root-1")
                    .append("not-stanza-thread")
                    .build(),
            )
            .build();

        assert_eq!(parse_thread_info(&xml), None);
    }

    #[test]
    fn parent_only_with_empty_id_is_rejected() {
        // XEP-0201: `parent` is meaningful only as a back-reference from a
        // thread that has its own id. A `<thread parent='X'/>` with no id is
        // ill-formed; this helper rejects it so the write path never persists
        // a parent without a thread id.
        let xml = "<message xmlns='jabber:client'><thread parent='root-1'></thread></message>"
            .parse::<Element>()
            .expect("valid xml");
        assert_eq!(parse_thread_info(&xml), None);
    }

    fn tid(value: &str) -> ThreadId {
        ThreadId::new(value).expect("non-empty")
    }

    #[test]
    fn build_element_with_parent() {
        let info = ThreadInfo::child(tid("child-a"), tid("root-a"));
        let elem = build_thread_element(&info, "jabber:client");
        assert_eq!(elem.name(), THREAD_ELEMENT);
        assert_eq!(elem.ns(), "jabber:client");
        assert_eq!(elem.attr("parent"), Some("root-a"));
        assert_eq!(elem.text(), "child-a");
    }

    #[test]
    fn build_element_without_parent() {
        let info = ThreadInfo::root(tid("root-a"));
        let elem = build_thread_element(&info, "jabber:client");
        assert_eq!(elem.attr("parent"), None);
        assert_eq!(elem.text(), "root-a");
    }

    #[test]
    fn install_thread_element_inherits_message_ns() {
        let mut xml = "<message xmlns='jabber:client'/>"
            .parse::<Element>()
            .expect("valid xml");
        install_thread_element(&mut xml, &ThreadInfo::child(tid("c"), tid("r")));
        let thread = xml
            .children()
            .find(|c| c.name() == THREAD_ELEMENT)
            .expect("thread child");
        assert_eq!(thread.ns(), "jabber:client");
    }

    #[test]
    fn install_thread_element_strips_existing() {
        let mut xml = "<message xmlns='jabber:client'><thread>old</thread></message>"
            .parse::<Element>()
            .expect("valid xml");
        install_thread_element(&mut xml, &ThreadInfo::child(tid("new"), tid("root")));
        let count = xml
            .children()
            .filter(|c| c.name() == THREAD_ELEMENT)
            .count();
        assert_eq!(count, 1);
        let info = parse_thread_info(&xml).expect("thread after install");
        assert_eq!(info.id.as_str(), "new");
        assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root"));
    }

    #[test]
    fn install_thread_element_preserves_unrelated_namespaced_thread_payload() {
        let mut xml = "<message xmlns='jabber:client'><thread xmlns='urn:example:other:0' kind='extension'>keep me</thread><thread>old</thread></message>"
            .parse::<Element>()
            .expect("valid xml");
        install_thread_element(&mut xml, &ThreadInfo::child(tid("new"), tid("root")));

        assert!(xml.children().any(|c| {
            c.name() == THREAD_ELEMENT && c.ns() == "urn:example:other:0" && c.text() == "keep me"
        }));
        let count = xml
            .children()
            .filter(|c| is_thread_element_for_stanza(c, "jabber:client"))
            .count();
        assert_eq!(count, 1);
        let info = parse_thread_info(&xml).expect("thread after install");
        assert_eq!(info.id.as_str(), "new");
        assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root"));
    }

    #[test]
    fn install_thread_element_preserves_explicit_empty_namespace_thread_payload() {
        let mut xml = Element::builder("message", "jabber:client")
            .append(
                Element::builder(THREAD_ELEMENT, "")
                    .attr("kind", "extension")
                    .append("keep me")
                    .build(),
            )
            .append(
                Element::builder(THREAD_ELEMENT, "jabber:client")
                    .append("old")
                    .build(),
            )
            .build();

        install_thread_element(&mut xml, &ThreadInfo::child(tid("new"), tid("root")));

        assert!(xml
            .children()
            .any(|c| c.name() == THREAD_ELEMENT && c.ns().is_empty() && c.text() == "keep me"));
        let count = xml
            .children()
            .filter(|c| is_thread_element_for_stanza(c, "jabber:client"))
            .count();
        assert_eq!(count, 1);
        let info = parse_thread_info(&xml).expect("thread after install");
        assert_eq!(info.id.as_str(), "new");
        assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root"));
    }

    #[test]
    fn thread_feature_constant() {
        assert_eq!(NS_THREAD_FEATURE, "urn:xmpp:threads:0");
    }

    #[test]
    fn set_and_get_thread_id_round_trip() {
        let mut msg = Message::new(None::<jid::Jid>);
        assert_eq!(thread_id_from_message(&msg), None);
        set_thread_id(&mut msg, "thread-root-1");
        assert_eq!(
            thread_id_from_message(&msg).as_deref(),
            Some("thread-root-1")
        );
    }

    #[test]
    fn set_thread_id_overwrites() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_id(&mut msg, "first");
        set_thread_id(&mut msg, "second");
        assert_eq!(thread_id_from_message(&msg).as_deref(), Some("second"));
    }

    #[test]
    fn thread_info_from_message_recovers_parent_from_payload_form() {
        // Post-`reattach_thread_parent` invariant: parent attribute lives in
        // `msg.payloads` as a raw element rather than `msg.thread`, because
        // `xmpp_parsers::Thread(String)` drops it at parse time.
        let mut msg = Message::new(None::<jid::Jid>);
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, "jabber:client")
                .attr("parent", "root-1")
                .append("child-2")
                .build(),
        );
        let info = thread_info_from_message(&msg).expect("thread info");
        assert_eq!(info.id.as_str(), "child-2");
        assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root-1"));
    }

    #[test]
    fn thread_info_from_message_falls_back_to_typed_field_when_no_payload() {
        // No payload thread element; the typed field is the only source.
        // Parent is unrecoverable in this branch by design — `xmpp_parsers`
        // dropped it at parse time and `reattach_thread_parent` was not run.
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_id(&mut msg, "abc");
        let info = thread_info_from_message(&msg).expect("thread info");
        assert_eq!(info.id.as_str(), "abc");
        assert_eq!(info.parent, None);
    }

    #[test]
    fn thread_info_from_message_payload_takes_precedence_over_typed_field() {
        // If both forms are present (transient pre-reattach state), the
        // payload form wins because it carries the parent attribute.
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_id(&mut msg, "stale-id");
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, "jabber:client")
                .attr("parent", "root-1")
                .append("authoritative-id")
                .build(),
        );
        let info = thread_info_from_message(&msg).expect("thread info");
        assert_eq!(info.id.as_str(), "authoritative-id");
        assert_eq!(info.parent.as_ref().map(ThreadId::as_str), Some("root-1"));
    }

    #[test]
    fn thread_info_from_message_ignores_empty_namespace_payload() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_id(&mut msg, "typed-thread");
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, "")
                .attr("parent", "foreign-root")
                .append("foreign-thread")
                .build(),
        );

        let info = thread_info_from_message(&msg).expect("typed thread");
        assert_eq!(info.id.as_str(), "typed-thread");
        assert_eq!(info.parent, None);
    }

    #[test]
    fn thread_info_from_message_ignores_wrong_stanza_namespace_payload() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_thread_id(&mut msg, "typed-thread");
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, SERVER_STANZA_NS)
                .attr("parent", "foreign-root")
                .append("foreign-thread")
                .build(),
        );

        let info = thread_info_from_message(&msg).expect("typed thread");
        assert_eq!(info.id.as_str(), "typed-thread");
        assert_eq!(info.parent, None);
    }

    #[test]
    fn thread_info_from_message_can_read_server_stanza_namespace() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, SERVER_STANZA_NS)
                .attr("parent", "server-root")
                .append("server-child")
                .build(),
        );

        assert_eq!(thread_info_from_message(&msg), None);
        let info =
            thread_info_from_message_in_stanza_ns(&msg, SERVER_STANZA_NS).expect("server thread");
        assert_eq!(info.id.as_str(), "server-child");
        assert_eq!(
            info.parent.as_ref().map(ThreadId::as_str),
            Some("server-root")
        );
    }

    #[test]
    fn thread_id_from_message_ignores_empty_namespace_payload() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, "")
                .append("foreign-thread")
                .build(),
        );

        assert_eq!(thread_id_from_message(&msg), None);
    }

    #[test]
    fn thread_info_from_message_payload_with_empty_id_is_rejected() {
        let mut msg = Message::new(None::<jid::Jid>);
        msg.payloads.push(
            Element::builder(THREAD_ELEMENT, "jabber:client")
                .attr("parent", "root-1")
                .build(),
        );
        assert_eq!(thread_info_from_message(&msg), None);
    }

    #[test]
    fn thread_info_from_message_returns_none_when_no_thread_at_all() {
        let msg = Message::new(None::<jid::Jid>);
        assert_eq!(thread_info_from_message(&msg), None);
    }
}
