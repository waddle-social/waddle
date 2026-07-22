//! XEP-0501 Pubsub Stories — wasm bridge surface.
//!
//! The server bootstraps a community stories node at
//! `urn:xmpp:pubsub-social-feed:stories:0` on the community service. These wasm methods
//! let the chat read active stories (`stories_items`, returns ALL
//! items; the chat filters expired locally) and publish new stories
//! with an expiry (`stories_publish`).

use chrono::{DateTime, Duration, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result,
};
use waddle_xmpp_core::xep0501::{
    build_story_element, parse_story, Story, DEFAULT_EXPIRY_HOURS, NS_ATOM, PUBSUB_NODE_STORIES,
};
use wasm_bindgen::prelude::*;

use super::{js_error, send_iq_command, service_bare_jid, to_js_value, WaddleClient};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JsStory {
    pub id: String,
    pub body: Option<String>,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
    pub author: Option<String>,
    pub posted: Option<String>,
    pub expires: Option<String>,
}

impl From<Story> for JsStory {
    fn from(story: Story) -> Self {
        Self {
            id: story.id,
            body: story.body,
            media_url: story.media_url,
            media_type: story.media_type.map(|media_type| media_type.to_string()),
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
    pub media_type: Option<String>,
    pub author: Option<String>,
    pub expiry_hours: Option<i64>,
}

fn build_stories_items_iq(community_service: &jid::BareJid, max_items: Option<u32>) -> Element {
    let id = format!("stories-items-{}", Uuid::new_v4());
    build_pubsub_items_iq(&id, Some(community_service), PUBSUB_NODE_STORIES, max_items)
}

fn build_story_publish_iq(
    community_service: &jid::BareJid,
    item_id: &str,
    story: &Story,
) -> Element {
    let id = format!("stories-publish-{}", Uuid::new_v4());
    build_pubsub_publish_iq(
        &id,
        Some(community_service),
        PUBSUB_NODE_STORIES,
        Some(item_id),
        build_story_element(story),
        None,
    )
}

fn parse_stories_items_result(iq: &Element) -> Vec<JsStory> {
    parse_pubsub_items_result(iq)
        .into_iter()
        .filter_map(|item| {
            let story_el = item.payload("entry", NS_ATOM)?;
            parse_story(&item.id, story_el).map(JsStory::from)
        })
        .collect()
}

#[wasm_bindgen]
impl WaddleClient {
    /// Fetch the latest stories from the community stories node on
    /// `community_jid`. Returns ALL items including expired ones —
    /// the chat filters active vs expired locally so a story fades
    /// out as the countdown hits zero without a server roundtrip.
    pub fn stories_items(&self, community_jid: String, max_items: Option<u32>) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let iq = build_stories_items_iq(&service, max_items);
            let result = send_iq_command(inner, iq).await?;
            let stories = parse_stories_items_result(&result);
            to_js_value(&stories)
        })
    }

    /// Publish a new story. `media_url` is required by XEP-0501; `body`
    /// is optional text content attached to that media. `expiry_hours`
    /// defaults to 24.
    pub fn stories_publish(&self, community_jid: String, input: JsValue) -> js_sys::Promise {
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let service = service_bare_jid(&community_jid)?;
            let input: JsStoryInput = serde_wasm_bindgen::from_value(input)
                .map_err(|err| js_error(format!("invalid story input: {err}")))?;
            let media_url = input
                .media_url
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty())
                .ok_or_else(|| js_error("story must have media_url"))?;
            let item_id = format!("story-{}", Uuid::new_v4());
            let mut story = Story::new(item_id.clone());
            if let Some(body) = input.body {
                story = story.with_body(body);
            }
            story = story.with_media(media_url);
            if let Some(media_type) = input.media_type {
                story = story.with_media_type(
                    media_type
                        .parse()
                        .map_err(|_| js_error("invalid story media_type"))?,
                );
            }
            if let Some(author) = input.author {
                story = story.with_author(author);
            }
            let hours = input.expiry_hours.unwrap_or(DEFAULT_EXPIRY_HOURS).max(1);
            story = story.with_expiry(Duration::hours(hours));
            let iq = build_story_publish_iq(&service, &item_id, &story);
            send_iq_command(inner, iq).await?;
            let posted: DateTime<Utc> = story.posted.unwrap_or_else(Utc::now);
            let expires: DateTime<Utc> = story.expires.expect("with_expiry always sets expires");
            let js_story = JsStory {
                id: item_id,
                body: story.body,
                media_url: story.media_url,
                media_type: story.media_type.map(|media_type| media_type.to_string()),
                author: story.author,
                posted: Some(posted.to_rfc3339()),
                expires: Some(expires.to_rfc3339()),
            };
            to_js_value(&js_story)
        })
    }
}
