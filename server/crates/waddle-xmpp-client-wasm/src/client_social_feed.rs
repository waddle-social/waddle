//! XEP-0472 Pubsub Social Feed — wasm bridge surface.
//!
//! The server bootstraps a community feed node at
//! `urn:xmpp:pubsub-social-feed:0` on the spaces service. These
//! wasm methods let the chat read the feed (`feed_items`) and
//! publish new posts (`feed_publish`) via standard XEP-0060
//! pubsub IQs, with the typed XEP-0472 `<entry/>` payload
//! constructed / parsed in Rust via `waddle_xmpp_core::xep0472`.

use chrono::{DateTime, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_core::xep0472::{
    build_feed_entry_element, parse_feed_entry, FeedEntry, NS_SOCIAL_FEED, PUBSUB_NODE_FEED,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, to_js_value, WaddleClient};
use crate::NS_CLIENT;

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// JS-facing snapshot of a XEP-0472 feed entry. Mirrors `FeedEntry`
/// but with stringly-typed `published` so it serialises cleanly
/// across the wasm boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsFeedEntry {
    pub id: String,
    pub title: Option<String>,
    pub body: String,
    pub author: Option<String>,
    /// RFC3339 string (or `None` if absent).
    pub published: Option<String>,
    pub link: Option<String>,
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

fn build_feed_items_iq(spaces_jid: &str, max_items: Option<u32>) -> Element {
    let id = format!("feed-items-{}", Uuid::new_v4());
    let mut items_builder = Element::builder("items", NS_PUBSUB).attr("node", PUBSUB_NODE_FEED);
    let max_items_value;
    if let Some(max) = max_items {
        max_items_value = max.to_string();
        items_builder = items_builder.attr("max_items", max_items_value.as_str());
    }
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(items_builder.build())
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "get")
        .attr("id", id)
        .attr("to", spaces_jid)
        .append(pubsub)
        .build()
}

fn build_feed_publish_iq(spaces_jid: &str, item_id: &str, entry: &FeedEntry) -> Element {
    let id = format!("feed-publish-{}", Uuid::new_v4());
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .append(build_feed_entry_element(entry))
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", PUBSUB_NODE_FEED)
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "set")
        .attr("id", id)
        .attr("to", spaces_jid)
        .append(pubsub)
        .build()
}

fn parse_feed_items_result(iq: &Element) -> Vec<JsFeedEntry> {
    let Some(pubsub) = iq.get_child("pubsub", NS_PUBSUB) else {
        return Vec::new();
    };
    let Some(items) = pubsub.get_child("items", NS_PUBSUB) else {
        return Vec::new();
    };
    items
        .children()
        .filter(|el| el.name() == "item" && el.ns() == NS_PUBSUB)
        .filter_map(|item| {
            let item_id = item.attr("id")?;
            let entry_el = item
                .children()
                .find(|child| child.name() == "entry" && child.ns() == NS_SOCIAL_FEED)?;
            parse_feed_entry(item_id, entry_el).map(JsFeedEntry::from)
        })
        .collect()
}

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest items from the community Social Feed node on
    /// `spaces_jid` (typically `spaces.<domain>`). Returns an array of
    /// JsFeedEntry objects ordered as the server delivered them
    /// (newest first by `last_published`).
    pub fn feed_items(&self, spaces_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let iq = build_feed_items_iq(&spaces_jid, max_items);
            let result = send_iq_command(inner, iq).await?;
            let entries = parse_feed_items_result(&result);
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
            let iq = build_feed_publish_iq(&spaces_jid, &item_id, &feed_entry);
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
            };
            to_js_value(&js_entry)
        })
    }
}
