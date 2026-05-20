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
//! XEP-0201 is Informational and does NOT define a disco#info feature.
//! Waddle therefore does not advertise a `urn:xmpp:threads:*` namespace —
//! `<thread/>` support is implicit in RFC-6121 conformance and the parent=
//! attribute is documented purely in this XEP.

use crate::mam::ThreadId;
use minidom::Element;
use serde::{Deserialize, Serialize};
use xmpp_parsers::message::{Message, Thread};

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
        .and_then(|thread| ThreadId::new(thread.id.clone()))
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
        builder = builder.attr(minidom::rxml::xml_ncname!("parent").to_owned(), parent);
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
        return Some(thread.id.clone());
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
    msg.thread = Some(Thread {
        id: thread_id.into(),
        parent: None,
    });
}

#[cfg(test)]
mod tests;
