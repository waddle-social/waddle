//! XEP-0501 Pubsub Stories — wasm bridge surface.
//!
//! The server bootstraps a community stories node at
//! `urn:xmpp:stories:0` on the spaces service. These wasm methods
//! let the chat read active stories (`stories_items`, filters
//! expired locally) and publish new stories with an expiry
//! (`stories_publish`) via standard XEP-0060 pubsub IQs, with the
//! typed XEP-0501 `<story/>` payload constructed / parsed in Rust
//! via `waddle_xmpp_core::xep0501`.

use chrono::{DateTime, Duration, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_core::xep0501::{
    build_story_element, parse_story, Story, DEFAULT_EXPIRY_HOURS, NS_STORIES, PUBSUB_NODE_STORIES,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, to_js_value, WaddleClient};
use crate::NS_CLIENT;

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

/// JS-facing snapshot of a XEP-0501 story. Mirrors `Story` with
/// RFC3339 strings on the timestamp fields so it serialises cleanly
/// across the wasm boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStory {
    pub id: String,
    pub body: Option<String>,
    pub media_url: Option<String>,
    pub author: Option<String>,
    /// RFC3339 string (or `None` if absent).
    pub posted: Option<String>,
    /// RFC3339 string (or `None` if absent).
    pub expires: Option<String>,
}

impl From<Story> for JsStory {
    fn from(story: Story) -> Self {
        Self {
            id: story.id,
            body: story.body,
            media_url: story.media_url,
            author: story.author,
            posted: story.posted.map(|ts| ts.to_rfc3339()),
            expires: story.expires.map(|ts| ts.to_rfc3339()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JsStoryInput {
    pub body: Option<String>,
    pub media_url: Option<String>,
    pub author: Option<String>,
    /// Hours from now until the story expires. Defaults to
    /// `DEFAULT_EXPIRY_HOURS` (24) when absent.
    pub expiry_hours: Option<i64>,
}

fn build_stories_items_iq(spaces_jid: &str, max_items: Option<u32>) -> Element {
    let id = format!("stories-items-{}", Uuid::new_v4());
    let mut items_builder = Element::builder("items", NS_PUBSUB).attr("node", PUBSUB_NODE_STORIES);
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

fn build_story_publish_iq(spaces_jid: &str, item_id: &str, story: &Story) -> Element {
    let id = format!("stories-publish-{}", Uuid::new_v4());
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", item_id)
        .append(build_story_element(story))
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", PUBSUB_NODE_STORIES)
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

fn parse_stories_items_result(iq: &Element) -> Vec<JsStory> {
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
            let story_el = item
                .children()
                .find(|child| child.name() == "story" && child.ns() == NS_STORIES)?;
            parse_story(item_id, story_el).map(JsStory::from)
        })
        .collect()
}

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest items from the community stories node on
    /// `spaces_jid`. Returns ALL items including expired ones —
    /// the chat client filters active vs expired locally so it can
    /// fade out a story right as the countdown hits zero without a
    /// server round-trip.
    pub fn stories_items(&self, spaces_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let iq = build_stories_items_iq(&spaces_jid, max_items);
            let result = send_iq_command(inner, iq).await?;
            let stories = parse_stories_items_result(&result);
            to_js_value(&stories)
        })
    }

    /// Publish a new story to the community stories node. Body
    /// and/or media URL must be provided; the server stamps the
    /// publisher's JID separately. `expiry_hours` defaults to 24
    /// (matches XEP-0501 §"Default expiry").
    pub fn stories_publish(&self, spaces_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let input: JsStoryInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid story input: {err}")))?;
            if input.body.is_none() && input.media_url.is_none() {
                return Err(js_error("story must have body or media_url"));
            }
            let item_id = format!("story-{}", Uuid::new_v4());
            let mut story = Story::new(item_id.clone());
            if let Some(body) = input.body {
                story = story.with_body(body);
            }
            if let Some(url) = input.media_url {
                story = story.with_media(url);
            }
            if let Some(author) = input.author {
                story = story.with_author(author);
            }
            let hours = input.expiry_hours.unwrap_or(DEFAULT_EXPIRY_HOURS).max(1);
            story = story.with_expiry(Duration::hours(hours));
            let iq = build_story_publish_iq(&spaces_jid, &item_id, &story);
            send_iq_command(inner, iq).await?;
            let posted: DateTime<Utc> = story.posted.unwrap_or_else(Utc::now);
            let expires: DateTime<Utc> = story.expires.expect("with_expiry always sets expires");
            let js_story = JsStory {
                id: item_id,
                body: story.body,
                media_url: story.media_url,
                author: story.author,
                posted: Some(posted.to_rfc3339()),
                expires: Some(expires.to_rfc3339()),
            };
            to_js_value(&js_story)
        })
    }
}
