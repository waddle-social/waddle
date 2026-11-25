use crate::{invalid_input, InvalidInput};
use chrono::{Duration, Utc};
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp_client::pubsub::{
    build_pubsub_items_iq, build_pubsub_publish_iq, parse_pubsub_items_result,
};
use waddle_xmpp_core::xep0501::{
    build_story_element, parse_story, Story, DEFAULT_EXPIRY_HOURS, NS_ATOM, PUBSUB_NODE_STORIES,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaddleStory {
    pub id: String,
    pub body: Option<String>,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
    pub author: Option<String>,
    pub posted: Option<String>,
    pub expires: Option<String>,
}

impl From<Story> for WaddleStory {
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
pub struct WaddleStoryInput {
    pub body: Option<String>,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
    pub author: Option<String>,
    pub expiry_hours: Option<i64>,
}

pub fn build_stories_items_iq(community_service: &jid::BareJid, max_items: Option<u32>) -> Element {
    let id = format!("stories-items-{}", Uuid::new_v4());
    build_pubsub_items_iq(&id, Some(community_service), PUBSUB_NODE_STORIES, max_items)
}

fn build_story_publish_iq(service: Option<&jid::BareJid>, item_id: &str, story: &Story) -> Element {
    let id = format!("stories-publish-{}", Uuid::new_v4());
    build_pubsub_publish_iq(
        &id,
        service,
        PUBSUB_NODE_STORIES,
        Some(item_id),
        build_story_element(story),
        None,
    )
}

pub fn parse_stories_items_result(iq: &Element) -> Vec<WaddleStory> {
    parse_pubsub_items_result(iq)
        .into_iter()
        .filter_map(|item| {
            let story_el = item.payload("entry", NS_ATOM)?;
            parse_story(&item.id, story_el).map(WaddleStory::from)
        })
        .collect()
}

pub fn prepare_publication(
    service: Option<&jid::BareJid>,
    input: WaddleStoryInput,
) -> Result<(Element, WaddleStory), InvalidInput> {
    let media_url = input
        .media_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| invalid_input("story must have media_url"))?;
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
                .map_err(|_| invalid_input("invalid story media_type"))?,
        );
    }
    if let Some(author) = input.author {
        story = story.with_author(author);
    }
    let hours = input.expiry_hours.unwrap_or(DEFAULT_EXPIRY_HOURS).max(1);
    let duration =
        Duration::try_hours(hours).ok_or_else(|| invalid_input("expiry out of range"))?;
    let expires = Utc::now()
        .checked_add_signed(duration)
        .ok_or_else(|| invalid_input("expiry out of range"))?;
    story = story.with_expires_at(expires);
    let iq = build_story_publish_iq(service, &item_id, &story);
    Ok((iq, WaddleStory::from(story)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

    fn service() -> jid::BareJid {
        "community.waddle.test".parse().expect("valid service jid")
    }

    #[test]
    fn stories_items_iq_pins_the_xep0060_wire_shape() {
        let iq = build_stories_items_iq(&service(), Some(100));
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("community.waddle.test"));
        assert!(iq.attr("id").expect("iq id").starts_with("stories-items-"));
        let items = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
            .expect("items element");
        assert_eq!(items.attr("node"), Some(PUBSUB_NODE_STORIES));
        assert_eq!(items.attr("max_items"), Some("100"));
    }

    #[test]
    fn story_publish_iq_pins_the_atom_entry_publish_shape() {
        let story = Story::new("story-1");
        let iq = build_story_publish_iq(Some(&service()), "story-1", &story);
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), Some("community.waddle.test"));
        assert!(iq
            .attr("id")
            .expect("iq id")
            .starts_with("stories-publish-"));
        let publish = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
            .expect("publish element");
        assert_eq!(publish.attr("node"), Some(PUBSUB_NODE_STORIES));
        let item = publish.get_child("item", NS_PUBSUB).expect("item element");
        assert_eq!(item.attr("id"), Some("story-1"));
        assert!(item.get_child("entry", NS_ATOM).is_some());
    }
}
