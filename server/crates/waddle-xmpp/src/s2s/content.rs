//! The content namespace of stanzas crossing a server-to-server stream.
//!
//! RFC 6120 §4.8.3: stanzas on a server-to-server stream are qualified by
//! `jabber:server`, as are their standard children (`body`, `subject`,
//! `thread`, `show`, `status`, `priority` and `error`); on a client stream
//! the same elements are qualified by `jabber:client`. Typed stanzas are
//! built and parsed in `jabber:client`, so a stanza crossing a
//! server-to-server stream has its content namespace mapped at the stream
//! boundary, in both directions.
//!
//! Only the stanza element and its direct children are mapped. A stanza
//! nested in a payload, such as an XEP-0297 forwarded message, keeps the
//! namespace it declares.

use minidom::{Element, Node};

use crate::ns::{JABBER_CLIENT, JABBER_SERVER};

/// `stanza` qualified by `jabber:server`, for a server-to-server stream.
pub fn into_server_content(stanza: Element) -> Element {
    remap(stanza, JABBER_CLIENT, JABBER_SERVER)
}

/// `stanza`, received on a server-to-server stream, qualified by
/// `jabber:client`, for typed parsing.
pub fn into_client_content(stanza: Element) -> Element {
    remap(stanza, JABBER_SERVER, JABBER_CLIENT)
}

fn remap(mut stanza: Element, from: &str, to: &str) -> Element {
    let mut mapped = renamed(&stanza, from, to);
    for node in stanza.take_nodes() {
        match node {
            Node::Element(mut child) if child.ns() == from => {
                let mut mapped_child = renamed(&child, from, to);
                for grandchild in child.take_nodes() {
                    mapped_child.append_node(grandchild);
                }
                mapped.append_child(mapped_child);
            }
            other => mapped.append_node(other),
        }
    }
    mapped
}

/// An empty copy of `element` with its attributes, qualified by `to` when
/// it was qualified by `from`.
fn renamed(element: &Element, from: &str, to: &str) -> Element {
    let namespace = element.ns();
    let namespace = if namespace == from {
        to.to_owned()
    } else {
        namespace
    };
    let mut copy = Element::bare(element.name(), namespace);
    *copy.attrs_mut() = element.attrs().clone();
    copy
}
