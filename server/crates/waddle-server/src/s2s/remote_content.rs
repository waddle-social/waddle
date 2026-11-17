//! What this server removes from a message another server sent before any
//! local client, archive or store sees it (ADR-0020 D7; gate G18).
//!
//! A client reads some payloads as its own server's statements about its
//! own account: the Waddle inbox elements, the DM pin marker, a XEP-0280
//! `<received/>` or `<sent/>` carbon wrapper, a XEP-0313 `<result/>` of its
//! archive, a XEP-0359 `<stanza-id/>` in its archive's name, and a XEP-0511
//! link card, which this server stamps after removing any a local sender
//! supplied (a client plays an inline video only from a URL a card names).
//! A remote entity that sends one forges it (XEP-0280 §11; XEP-0511,
//! Security Considerations), so [`sanitize_message`] removes them from every
//! message that crosses S2S to a local account: a remote account's or
//! service's message, a PubSub event notification (inside its items too),
//! and a remote room's groupchat, private messages and subject. No card is
//! stamped in place of a removed one: this server resolves no preview for a
//! remote sender, whose request token it could not have issued.
//!
//! A remote room's archive answers a query the local resource sent it with
//! `<result/>` messages from the room's bare JID (XEP-0313 §4.1.2), which a
//! client matches to its query by `queryid`; they are kept, and what they
//! forward is cleaned as any nested content is. A `<result/>` from any other
//! address is removed.
//!
//! Presence and IQ payloads are not cleaned here: a client reads none of
//! these payloads from them.

use jid::{BareJid, Jid};
use minidom::{Element, Node};
use waddle_xmpp::carbons::CARBONS_NS;
use waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0;
use waddle_xmpp::xep::{
    is_link_metadata_element, is_link_preview_request_element, NS_INBOX, NS_WADDLE_INBOX,
};
use waddle_xmpp_core::mam::MAM_NS;
use xmpp_parsers::message::Message;

use crate::destination::{ServedDomains, UnservedDestination};

/// The namespace prefix of every payload this deployment defines. None is
/// relayed between servers, so a remote message carries one only to pass
/// it off as this server's.
const WADDLE_NAMESPACE_PREFIX: &str = "urn:waddle:";

/// Where a message from another server comes from.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RemoteSource<'a> {
    /// A remote account, a remote service, or a room the recipient
    /// occupies no seat in.
    Account,
    /// The remote room `room` the recipient occupies or is joining: its
    /// bare JID is the archive a `<result/>` may come from.
    Room(&'a BareJid),
}

/// Remove from `message` every payload a client would read as this
/// server's, at its top level and nested in what it keeps. `served` names
/// the domains whose `<stanza-id/>` a remote entity cannot have assigned;
/// without it every `<stanza-id/>` is removed.
pub(crate) fn sanitize_message(
    message: &mut Message,
    served: Option<&ServedDomains>,
    source: RemoteSource<'_>,
) {
    let archive_results = match source {
        RemoteSource::Account => false,
        RemoteSource::Room(room) => message.from.as_ref() == Some(&Jid::from(room.clone())),
    };
    message.payloads.retain(|payload| {
        if payload.is("result", MAM_NS) {
            return archive_results;
        }
        !payload.ns().starts_with(WADDLE_NAMESPACE_PREFIX) && !private(payload, served)
    });
    for payload in &mut message.payloads {
        sanitize_children(payload, served);
    }
}

/// Whether `element`, wherever it is in a remote message, is a payload a
/// client reads as this server's.
fn private(element: &Element, served: Option<&ServedDomains>) -> bool {
    let namespace = element.ns();
    is_link_metadata_element(element)
        || is_link_preview_request_element(element)
        || namespace == NS_INBOX
        || namespace == NS_WADDLE_INBOX
        || namespace == NS_WADDLE_PIN_V0
        || namespace == waddle_extensions::FRAMEWORK_NAMESPACE
        || element.is("received", CARBONS_NS)
        || element.is("sent", CARBONS_NS)
        || element.is("result", MAM_NS)
        || assigned_here(element, served)
}

/// Remove from the children of `element`, at every depth, what [`private`]
/// names. The S2S codec bounds the depth of a stanza.
fn sanitize_children(element: &mut Element, served: Option<&ServedDomains>) {
    for node in element.take_nodes() {
        match node {
            Node::Element(mut child) => {
                if private(&child, served) {
                    continue;
                }
                sanitize_children(&mut child, served);
                element.append_node(Node::Element(child));
            }
            text @ Node::Text(_) => element.append_node(text),
        }
    }
}

/// Whether `element` is a XEP-0359 `<stanza-id/>` this deployment could
/// have assigned: one whose `by` names an address at a domain it serves
/// (the recipient account's archive among them), or that cannot be read as
/// an address. A client takes an id in its own account's name for the one
/// its archive gave the message; an entity that assigns stanza ids deletes
/// one whose `by` it would set (XEP-0359 §3), and the recipient pass
/// replaces only an id in the name it stamps itself. An id a remote entity
/// assigned in its own name is left, as a routing entity leaves it (§3).
fn assigned_here(element: &Element, served: Option<&ServedDomains>) -> bool {
    if !element.is("stanza-id", waddle_xmpp_core::xep0359::NS_SID) {
        return false;
    }
    let Some(served) = served else {
        return true;
    };
    let Some(by) = element.attr("by").and_then(|by| by.parse::<Jid>().ok()) else {
        return true;
    };
    !matches!(
        served.classify(Some(&by)).unserved(),
        Some(UnservedDestination::Remote(_))
    )
}
