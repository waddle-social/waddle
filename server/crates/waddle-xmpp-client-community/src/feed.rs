use chrono::Utc;
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result,
};
use waddle_xmpp_core::xep0472::{
    build_feed_entry_element, parse_feed_entry, FeedEntry, NS_ATOM, PUBSUB_NODE_FEED,
};
/// Waddle-namespaced typed child on bridged feed entries (PEP →
/// feed). Carries `kind="mood"|"activity"|"tune"|"avatar"|"vcard"`
/// so the chat can render a kind-icon next to bridged posts.
const NS_FEED_SOURCE: &str = "urn:waddle:feed-source:0";

/// JS-facing snapshot of a XEP-0472 feed entry. Mirrors `FeedEntry`
/// but with stringly-typed `published` so it serialises cleanly
/// across the wasm boundary, plus a derived `source` field that
/// captures the PEP-bridge kind (when present) so the chat can
/// distinguish bridged entries from manual posts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleFeedEntry {
    pub id: String,
    pub title: Option<String>,
    pub body: String,
    pub author: Option<String>,
    pub published: Option<String>,
    pub link: Option<String>,
    /// PEP-bridge source kind from the `<source xmlns='urn:waddle:
    /// feed-source:0' kind='...'/>` child. Present only on bridged
    /// entries; manual posts leave this `None`.
    pub source: Option<String>,
}

impl From<FeedEntry> for WaddleFeedEntry {
    fn from(entry: FeedEntry) -> Self {
        Self {
            id: entry.id,
            title: entry.title,
            body: entry.body,
            author: entry.author,
            published: entry.published.map(|ts| ts.to_rfc3339()),
            link: entry.link,
            source: None,
        }
    }
}

/// Extract the PEP-bridge source kind from the raw `<entry>` element
/// (`<source xmlns='urn:waddle:feed-source:0' kind='...'/>`) when
/// present. Returns `None` for manual posts that omit the child.
fn extract_source_kind(entry_el: &Element) -> Option<String> {
    entry_el
        .children()
        .find(|c| c.name() == "source" && c.ns() == NS_FEED_SOURCE)
        .and_then(|c| c.attr("kind"))
        .map(str::to_string)
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaddleFeedEntryInput {
    pub title: Option<String>,
    pub body: String,
    pub author: Option<String>,
    pub link: Option<String>,
}

pub fn build_feed_items_iq(community_service: &jid::BareJid, max_items: Option<u32>) -> Element {
    let id = format!("feed-items-{}", Uuid::new_v4());
    build_pubsub_items_iq(&id, Some(community_service), PUBSUB_NODE_FEED, max_items)
}

fn build_feed_publish_iq(
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
        build_feed_entry_element(entry),
        None,
    )
}

pub fn parse_feed_items_result(iq: &Element) -> Vec<WaddleFeedEntry> {
    parse_pubsub_items_result(iq)
        .into_iter()
        .filter_map(|item| {
            let entry_el = item.payload("entry", NS_ATOM)?;
            let source = extract_source_kind(entry_el);
            parse_feed_entry(&item.id, entry_el).map(|entry| {
                let mut js = WaddleFeedEntry::from(entry);
                js.source = source;
                js
            })
        })
        .collect()
}

pub fn prepare_publication(
    service: &jid::BareJid,
    input: WaddleFeedEntryInput,
) -> (Element, WaddleFeedEntry) {
    let item_id = format!("post-{}", Uuid::new_v4());
    let mut feed_entry = FeedEntry::new(item_id.clone(), input.body);
    if let Some(title) = input.title {
        feed_entry = feed_entry.with_title(title);
    }
    if let Some(author) = input.author {
        feed_entry = feed_entry.with_author(author);
    }
    if let Some(link) = input.link {
        feed_entry = feed_entry.with_link(link);
    }
    feed_entry = feed_entry.with_published(Utc::now());
    let iq = build_feed_publish_iq(service, &item_id, &feed_entry);
    (iq, WaddleFeedEntry::from(feed_entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

    fn service() -> jid::BareJid {
        "community.waddle.test".parse().expect("valid service jid")
    }

    #[test]
    fn feed_items_iq_pins_the_xep0060_wire_shape() {
        let iq = build_feed_items_iq(&service(), Some(50));
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("community.waddle.test"));
        assert!(iq.attr("id").expect("iq id").starts_with("feed-items-"));
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .expect("items element");
        assert_eq!(items.attr("node"), Some(PUBSUB_NODE_FEED));
        assert_eq!(items.attr("max_items"), Some("50"));

        let unbounded = build_feed_items_iq(&service(), None);
        let items = unbounded
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .expect("items element");
        assert_eq!(items.attr("max_items"), None);
    }

    #[test]
    fn feed_publish_iq_pins_the_atom_entry_publish_shape() {
        let entry = FeedEntry::new("post-1", "hello waddlers");
        let iq = build_feed_publish_iq(&service(), "post-1", &entry);
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), Some("community.waddle.test"));
        assert!(iq.attr("id").expect("iq id").starts_with("feed-publish-"));
        let publish = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
            .expect("publish element");
        assert_eq!(publish.attr("node"), Some(PUBSUB_NODE_FEED));
        let item = publish.get_child("item", NS_PUBSUB).expect("item element");
        assert_eq!(item.attr("id"), Some("post-1"));
        assert!(item.get_child("entry", NS_ATOM).is_some());
        // No publish-options precondition on feed posts.
        assert!(iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("publish-options", NS_PUBSUB))
            .is_none());
    }
}
