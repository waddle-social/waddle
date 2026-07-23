//! XEP-0472 Pubsub Social Feed — wasm bridge surface.
//!
//! The server bootstraps a community feed node at
//! `urn:xmpp:pubsub-social-feed:1` on the community service
//! (`community.<domain>`). These wasm methods let the chat read the
//! feed (`feed_items`) and publish new posts (`feed_publish`) via the
//! shared feed verbs in `waddle_xmpp_client::social_feed`, with the
//! typed XEP-0472 `<entry/>` payload constructed / parsed in Rust via
//! `waddle_xmpp_core::xep0472`.

use chrono::{DateTime, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::social_feed::{
    build_feed_items_iq, build_feed_publish_iq, parse_feed_items_result, FeedSourceKind,
};
use waddle_xmpp_core::xep0472::FeedEntry;
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};

/// JS-facing snapshot of a XEP-0472 feed entry. Mirrors `FeedEntry`
/// but with stringly-typed `published` so it serialises cleanly
/// across the wasm boundary, plus a derived `source` field that
/// captures the PEP-bridge kind (when present) so the chat can
/// distinguish bridged entries from manual posts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsFeedEntry {
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

impl From<FeedEntry> for JsFeedEntry {
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

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsFeedEntryInput {
    pub title: Option<String>,
    pub body: String,
    pub author: Option<String>,
    pub link: Option<String>,
}

fn feed_items_to_js(iq: &Element) -> Vec<JsFeedEntry> {
    parse_feed_items_result(iq)
        .into_iter()
        .map(|item| {
            let source = item.source.map(FeedSourceKind::as_str).map(str::to_owned);
            let mut js = JsFeedEntry::from(item.entry);
            js.source = source;
            js
        })
        .collect()
}

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest items from the community Social Feed node on
    /// `spaces_jid` — the community service (`community.<domain>`).
    /// Returns an array of JsFeedEntry objects ordered as the server
    /// delivered them (newest first by `last_published`).
    pub fn feed_items(&self, spaces_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&spaces_jid)?;
            let iq = build_feed_items_iq(&service, max_items);
            let result = send_iq_command(inner, iq).await?;
            let entries = feed_items_to_js(&result);
            to_js_value(&entries)
        })
    }

    /// Publish a new entry to the community Social Feed. The server
    /// enforces publish authorisation via XEP-0060 affiliations;
    /// callers without Publisher access receive a Forbidden stanza
    /// error which surfaces as a rejected Promise.
    pub fn feed_publish(&self, spaces_jid: String, entry: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&spaces_jid)?;
            let input: JsFeedEntryInput = serde_wasm_bindgen::from_value(entry)
                .map_err(|err| js_error(format!("invalid feed entry input: {err}")))?;
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
            let iq = build_feed_publish_iq(&service, &item_id, &feed_entry);
            send_iq_command(inner, iq).await?;
            // Return the entry we just published so the chat can append
            // it to local state without round-tripping through items.
            let published: DateTime<Utc> = feed_entry
                .published
                .expect("with_published always sets published");
            let js_entry = JsFeedEntry {
                id: item_id,
                title: feed_entry.title,
                body: feed_entry.body,
                author: feed_entry.author,
                published: Some(published.to_rfc3339()),
                link: feed_entry.link,
                source: None,
            };
            to_js_value(&js_entry)
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_core::xep0472::{NS_ATOM, PUBSUB_NODE_FEED};

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
