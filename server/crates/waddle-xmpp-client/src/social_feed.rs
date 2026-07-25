//! XEP-0472 Pubsub Social Feed — shared feed verbs.
//!
//! The server bootstraps a community feed node at
//! [`PUBSUB_NODE_FEED`] on the community service
//! (`community.<domain>`, see [`crate::pubsub::community_service_jid`]).
//! This module owns the typed feed operations every Waddle client
//! surface (wasm chat, Android/Apple FFI) shares: the items GET and
//! publish SET envelopes layered on the XEP-0060 helpers in
//! [`crate::pubsub`], plus the items-result parse into typed
//! [`FeedEntry`] values with the Waddle PEP-bridge source kind.
//!
//! The Atom `<entry/>` payload itself is built/parsed by
//! `waddle_xmpp_core::xep0472`; the `urn:waddle:feed-source:0`
//! `<source kind='…'/>` child on bridged entries (PEP → feed) is
//! modelled here as the closed [`FeedSourceKind`] enum.

use minidom::Element;
use uuid::Uuid;
use waddle_xmpp_core::xep0472::{parse_feed_entry, FeedEntry, NS_ATOM, PUBSUB_NODE_FEED};

use crate::pubsub::{build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result};

#[cfg(test)]
mod tests;

/// Waddle-namespaced typed child on bridged feed entries (PEP →
/// feed). Carries `kind="mood"|"activity"|"tune"|"avatar"|"vcard"|
/// "rsvp"` so clients can render a kind-icon next to bridged posts.
/// Legitimately custom: no XEP defines a bridge-source annotation for
/// XEP-0472 entries.
pub const NS_FEED_SOURCE: &str = "urn:waddle:feed-source:0";

/// Closed domain of `kind` values the server's PEP bridge stamps on
/// `<source xmlns='urn:waddle:feed-source:0'/>` children. Manual
/// posts carry no source child; unknown kinds parse to `None` so a
/// newer server never breaks an older client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedSourceKind {
    Mood,
    Activity,
    Tune,
    Avatar,
    Vcard,
    Rsvp,
}

impl FeedSourceKind {
    /// Parse a wire `kind` attribute value; unknown values yield
    /// `None` (forward compatibility with new bridge kinds).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "mood" => Some(Self::Mood),
            "activity" => Some(Self::Activity),
            "tune" => Some(Self::Tune),
            "avatar" => Some(Self::Avatar),
            "vcard" => Some(Self::Vcard),
            "rsvp" => Some(Self::Rsvp),
            _ => None,
        }
    }

    /// Wire `kind` attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mood => "mood",
            Self::Activity => "activity",
            Self::Tune => "tune",
            Self::Avatar => "avatar",
            Self::Vcard => "vcard",
            Self::Rsvp => "rsvp",
        }
    }
}

/// One parsed feed item: the typed Atom entry plus the PEP-bridge
/// source kind when the entry was bridged (manual posts leave it
/// `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    pub entry: FeedEntry,
    pub source: Option<FeedSourceKind>,
}

/// Extract the PEP-bridge source kind from a raw Atom `<entry/>`
/// element (`<source xmlns='urn:waddle:feed-source:0' kind='…'/>`).
/// Returns `None` for manual posts that omit the child and for
/// unknown kinds.
pub fn extract_source_kind(entry_el: &Element) -> Option<FeedSourceKind> {
    entry_el
        .children()
        .find(|c| c.name() == "source" && c.ns() == NS_FEED_SOURCE)
        .and_then(|c| c.attr("kind"))
        .and_then(FeedSourceKind::parse)
}

/// XEP-0060 items GET for the latest entries on the community feed
/// node. The IQ id is minted here with the conventional
/// `feed-items-` prefix.
pub fn build_feed_items_iq(community_service: &jid::BareJid, max_items: Option<u32>) -> Element {
    let id = format!("feed-items-{}", Uuid::new_v4());
    build_pubsub_items_iq(&id, Some(community_service), PUBSUB_NODE_FEED, max_items)
}

/// XEP-0060 publish SET for one Atom `<entry/>` on the community feed
/// node under the caller-supplied `item_id` (conventionally
/// `post-<uuid>`). The IQ id is minted here with the conventional
/// `feed-publish-` prefix. No publish-options precondition — the
/// server configures the feed node at bootstrap.
pub fn build_feed_publish_iq(
    community_service: &jid::BareJid,
    item_id: &str,
    entry: &FeedEntry,
) -> Element {
    let id = format!("feed-publish-{}", Uuid::new_v4());
    build_pubsub_publish_iq(
        &id,
        Some(community_service),
        PUBSUB_NODE_FEED,
        Some(item_id),
        waddle_xmpp_core::xep0472::build_feed_entry_element(entry),
        None,
    )
}

/// Parse a feed items result IQ into typed entries. Tolerant by
/// design (mirroring [`parse_pubsub_items_result`]): items without an
/// Atom `<entry/>` payload or with an unparsable entry are skipped.
pub fn parse_feed_items_result(iq: &Element) -> Vec<FeedItem> {
    parse_pubsub_items_result(iq)
        .into_iter()
        .filter_map(|item| {
            let entry_el = item.payload("entry", NS_ATOM)?;
            let source = extract_source_kind(entry_el);
            parse_feed_entry(&item.id, entry_el).map(|entry| FeedItem { entry, source })
        })
        .collect()
}
