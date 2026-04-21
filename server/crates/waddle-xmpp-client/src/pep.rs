//! PEP (Personal Eventing Protocol) user-state publish/subscribe.
//!
//! Covers XEP-0107 (User Mood), XEP-0108 (User Activity), XEP-0118 (User Tune).

use minidom::Element;
use uuid::Uuid;

use crate::client::ClientHandle;
use crate::error::ClientResult;

pub const NS_MOOD: &str = "http://jabber.org/protocol/mood";
pub const NS_ACTIVITY: &str = "http://jabber.org/protocol/activity";
pub const NS_TUNE: &str = "http://jabber.org/protocol/tune";
pub const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
pub const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";

const NS_CLIENT: &str = "jabber:client";

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct UserMood {
    pub mood: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserActivity {
    pub activity: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserTune {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub source: Option<String>,
    pub length: Option<u32>,
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PepItem {
    Mood(UserMood),
    Activity(UserActivity),
    Tune(UserTune),
    MoodCleared,
    ActivityCleared,
    TuneCleared,
}

// ── Inbound parser ────────────────────────────────────────────────────────────

/// Parse a PEP event from an incoming `<message>` stanza.
pub fn parse(element: &Element) -> Option<PepItem> {
    if element.name() != "message" {
        return None;
    }

    let event = element.get_child("event", NS_PUBSUB_EVENT)?;
    let items = event.get_child("items", NS_PUBSUB_EVENT)?;
    let node = items.attr("node")?;

    match node {
        NS_MOOD => parse_mood(items),
        NS_ACTIVITY => parse_activity(items),
        NS_TUNE => parse_tune(items),
        _ => None,
    }
}

fn parse_mood(items: &Element) -> Option<PepItem> {
    let item = items.get_child("item", NS_PUBSUB_EVENT)?;
    let mood_el = item.get_child("mood", NS_MOOD)?;

    // Find the first non-<text> child — that is the mood name element.
    let mood_name_el = mood_el.children().find(|c| c.name() != "text");

    let Some(mood_name_el) = mood_name_el else {
        return Some(PepItem::MoodCleared);
    };

    let mood = mood_name_el.name().to_string();
    let text = mood_el.get_child("text", NS_MOOD).map(|t| t.text());

    Some(PepItem::Mood(UserMood { mood, text }))
}

fn parse_activity(items: &Element) -> Option<PepItem> {
    let item = items.get_child("item", NS_PUBSUB_EVENT)?;
    let activity_el = item.get_child("activity", NS_ACTIVITY)?;

    let activity_name_el = activity_el.children().find(|c| c.name() != "text");

    let Some(activity_name_el) = activity_name_el else {
        return Some(PepItem::ActivityCleared);
    };

    let activity = activity_name_el.name().to_string();
    let text = activity_el.get_child("text", NS_ACTIVITY).map(|t| t.text());

    Some(PepItem::Activity(UserActivity { activity, text }))
}

fn parse_tune(items: &Element) -> Option<PepItem> {
    let item = items.get_child("item", NS_PUBSUB_EVENT)?;
    let tune_el = item.get_child("tune", NS_TUNE)?;

    let artist = tune_el.get_child("artist", NS_TUNE).map(|e| e.text());
    let title = tune_el.get_child("title", NS_TUNE).map(|e| e.text());
    let source = tune_el.get_child("source", NS_TUNE).map(|e| e.text());
    let length = tune_el
        .get_child("length", NS_TUNE)
        .and_then(|e| e.text().parse::<u32>().ok());
    let uri = tune_el.get_child("uri", NS_TUNE).map(|e| e.text());

    if artist.is_none() && title.is_none() && source.is_none() && length.is_none() && uri.is_none()
    {
        return Some(PepItem::TuneCleared);
    }

    Some(PepItem::Tune(UserTune {
        artist,
        title,
        source,
        length,
        uri,
    }))
}

// ── Outbound XML builders ─────────────────────────────────────────────────────

/// Build a `<mood>` payload element.
pub fn build_mood_element(mood: &str, text: Option<&str>) -> Element {
    let mut builder =
        Element::builder("mood", NS_MOOD).append(Element::builder(mood, NS_MOOD).build());

    if let Some(t) = text {
        builder = builder.append(Element::builder("text", NS_MOOD).append(t).build());
    }

    builder.build()
}

/// Build an empty `<mood>` element (for clearing).
pub fn build_mood_clear_element() -> Element {
    Element::builder("mood", NS_MOOD).build()
}

/// Build an `<activity>` payload element.
pub fn build_activity_element(activity: &str, text: Option<&str>) -> Element {
    let mut builder = Element::builder("activity", NS_ACTIVITY)
        .append(Element::builder(activity, NS_ACTIVITY).build());

    if let Some(t) = text {
        builder = builder.append(Element::builder("text", NS_ACTIVITY).append(t).build());
    }

    builder.build()
}

/// Build an empty `<activity>` element (for clearing).
pub fn build_activity_clear_element() -> Element {
    Element::builder("activity", NS_ACTIVITY).build()
}

/// Build a `<tune>` payload element.
pub fn build_tune_element(tune: &UserTune) -> Element {
    let mut builder = Element::builder("tune", NS_TUNE);

    if let Some(ref v) = tune.artist {
        builder = builder.append(
            Element::builder("artist", NS_TUNE)
                .append(v.as_str())
                .build(),
        );
    }
    if let Some(ref v) = tune.title {
        builder = builder.append(
            Element::builder("title", NS_TUNE)
                .append(v.as_str())
                .build(),
        );
    }
    if let Some(ref v) = tune.source {
        builder = builder.append(
            Element::builder("source", NS_TUNE)
                .append(v.as_str())
                .build(),
        );
    }
    if let Some(v) = tune.length {
        builder = builder.append(
            Element::builder("length", NS_TUNE)
                .append(v.to_string().as_str())
                .build(),
        );
    }
    if let Some(ref v) = tune.uri {
        builder = builder.append(Element::builder("uri", NS_TUNE).append(v.as_str()).build());
    }

    builder.build()
}

/// Build an empty `<tune>` element (for clearing).
pub fn build_tune_clear_element() -> Element {
    Element::builder("tune", NS_TUNE).build()
}

/// Wrap a payload in a PEP publish IQ stanza.
pub fn build_pep_publish_iq(id: &str, node: &str, payload: Element) -> Element {
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", "current")
        .append(payload)
        .build();

    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", node)
        .append(item)
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build();

    Element::builder("iq", NS_CLIENT)
        .attr("type", "set")
        .attr("id", id)
        .append(pubsub)
        .build()
}

/// Build a PEP retract IQ (publish with empty payload).
pub fn build_pep_clear_iq(id: &str, node: &str) -> Element {
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", "current")
        .build();

    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", node)
        .append(item)
        .build();

    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build();

    Element::builder("iq", NS_CLIENT)
        .attr("type", "set")
        .attr("id", id)
        .append(pubsub)
        .build()
}

// ── PepExt trait on ClientHandle ─────────────────────────────────────────────

pub trait PepExt {
    async fn publish_mood(&self, mood: &str, text: Option<&str>) -> ClientResult<()>;
    async fn clear_mood(&self) -> ClientResult<()>;
    async fn publish_activity(&self, activity: &str, text: Option<&str>) -> ClientResult<()>;
    async fn clear_activity(&self) -> ClientResult<()>;
    async fn publish_tune(
        &self,
        artist: Option<&str>,
        title: Option<&str>,
        source: Option<&str>,
        length: Option<u32>,
        uri: Option<&str>,
    ) -> ClientResult<()>;
    async fn clear_tune(&self) -> ClientResult<()>;
}

impl PepExt for ClientHandle {
    async fn publish_mood(&self, mood: &str, text: Option<&str>) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let payload = build_mood_element(mood, text);
        self.send_stanza(build_pep_publish_iq(&id, NS_MOOD, payload))
            .await
    }

    async fn clear_mood(&self) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let payload = build_mood_clear_element();
        self.send_stanza(build_pep_publish_iq(&id, NS_MOOD, payload))
            .await
    }

    async fn publish_activity(&self, activity: &str, text: Option<&str>) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let payload = build_activity_element(activity, text);
        self.send_stanza(build_pep_publish_iq(&id, NS_ACTIVITY, payload))
            .await
    }

    async fn clear_activity(&self) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let payload = build_activity_clear_element();
        self.send_stanza(build_pep_publish_iq(&id, NS_ACTIVITY, payload))
            .await
    }

    async fn publish_tune(
        &self,
        artist: Option<&str>,
        title: Option<&str>,
        source: Option<&str>,
        length: Option<u32>,
        uri: Option<&str>,
    ) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let tune = UserTune {
            artist: artist.map(str::to_string),
            title: title.map(str::to_string),
            source: source.map(str::to_string),
            length,
            uri: uri.map(str::to_string),
        };
        let payload = build_tune_element(&tune);
        self.send_stanza(build_pep_publish_iq(&id, NS_TUNE, payload))
            .await
    }

    async fn clear_tune(&self) -> ClientResult<()> {
        let id = Uuid::new_v4().to_string();
        let payload = build_tune_clear_element();
        self.send_stanza(build_pep_publish_iq(&id, NS_TUNE, payload))
            .await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pep_message(node: &str, payload: Element) -> Element {
        let item = Element::builder("item", NS_PUBSUB_EVENT)
            .attr("id", "current")
            .append(payload)
            .build();

        let items = Element::builder("items", NS_PUBSUB_EVENT)
            .attr("node", node)
            .append(item)
            .build();

        let event = Element::builder("event", NS_PUBSUB_EVENT)
            .append(items)
            .build();

        Element::builder("message", NS_CLIENT)
            .attr("from", "user@example.com")
            .append(event)
            .build()
    }

    fn make_pep_message_empty_items(node: &str, payload: Element) -> Element {
        let item = Element::builder("item", NS_PUBSUB_EVENT)
            .attr("id", "current")
            .append(payload)
            .build();

        let items = Element::builder("items", NS_PUBSUB_EVENT)
            .attr("node", node)
            .append(item)
            .build();

        let event = Element::builder("event", NS_PUBSUB_EVENT)
            .append(items)
            .build();

        Element::builder("message", NS_CLIENT)
            .attr("from", "user@example.com")
            .append(event)
            .build()
    }

    #[test]
    fn parse_mood_happy_path() {
        let mood_el = Element::builder("mood", NS_MOOD)
            .append(Element::builder("happy", NS_MOOD).build())
            .append(
                Element::builder("text", NS_MOOD)
                    .append("feeling good")
                    .build(),
            )
            .build();

        let msg = make_pep_message(NS_MOOD, mood_el);
        let result = parse(&msg);

        assert_eq!(
            result,
            Some(PepItem::Mood(UserMood {
                mood: "happy".to_string(),
                text: Some("feeling good".to_string()),
            }))
        );
    }

    #[test]
    fn parse_mood_cleared() {
        let mood_el = Element::builder("mood", NS_MOOD).build();
        let msg = make_pep_message_empty_items(NS_MOOD, mood_el);
        let result = parse(&msg);
        assert_eq!(result, Some(PepItem::MoodCleared));
    }

    #[test]
    fn parse_tune_full() {
        let tune_el = Element::builder("tune", NS_TUNE)
            .append(
                Element::builder("artist", NS_TUNE)
                    .append("Artist Name")
                    .build(),
            )
            .append(
                Element::builder("title", NS_TUNE)
                    .append("Song Title")
                    .build(),
            )
            .append(Element::builder("source", NS_TUNE).append("Album").build())
            .append(Element::builder("length", NS_TUNE).append("213").build())
            .append(
                Element::builder("uri", NS_TUNE)
                    .append("https://example.com/song")
                    .build(),
            )
            .build();

        let msg = make_pep_message(NS_TUNE, tune_el);
        let result = parse(&msg);

        assert_eq!(
            result,
            Some(PepItem::Tune(UserTune {
                artist: Some("Artist Name".to_string()),
                title: Some("Song Title".to_string()),
                source: Some("Album".to_string()),
                length: Some(213),
                uri: Some("https://example.com/song".to_string()),
            }))
        );
    }

    #[test]
    fn parse_tune_cleared() {
        let tune_el = Element::builder("tune", NS_TUNE).build();
        let msg = make_pep_message(NS_TUNE, tune_el);
        let result = parse(&msg);
        assert_eq!(result, Some(PepItem::TuneCleared));
    }

    #[test]
    fn parse_activity_happy_path() {
        let activity_el = Element::builder("activity", NS_ACTIVITY)
            .append(Element::builder("exercising", NS_ACTIVITY).build())
            .append(
                Element::builder("text", NS_ACTIVITY)
                    .append("at the gym")
                    .build(),
            )
            .build();

        let msg = make_pep_message(NS_ACTIVITY, activity_el);
        let result = parse(&msg);

        assert_eq!(
            result,
            Some(PepItem::Activity(UserActivity {
                activity: "exercising".to_string(),
                text: Some("at the gym".to_string()),
            }))
        );
    }

    #[test]
    fn parse_ignores_non_pep_message() {
        let msg = Element::builder("message", NS_CLIENT)
            .attr("type", "chat")
            .attr("from", "user@example.com")
            .append(Element::builder("body", NS_CLIENT).append("Hello!").build())
            .build();

        assert_eq!(parse(&msg), None);
    }

    #[test]
    fn parse_ignores_iq() {
        let iq = Element::builder("iq", NS_CLIENT)
            .attr("type", "get")
            .attr("id", "some-id")
            .build();

        assert_eq!(parse(&iq), None);
    }

    #[test]
    fn build_mood_element_has_correct_ns() {
        let el = build_mood_element("happy", Some("yay"));
        assert_eq!(el.ns(), NS_MOOD);
        assert_eq!(el.name(), "mood");
        let mood_child = el.get_child("happy", NS_MOOD);
        assert!(mood_child.is_some());
        let text_child = el.get_child("text", NS_MOOD);
        assert_eq!(text_child.map(|t| t.text()), Some("yay".to_string()));
    }

    #[test]
    fn build_pep_publish_iq_structure() {
        let payload = build_mood_element("content", None);
        let iq = build_pep_publish_iq("test-id", NS_MOOD, payload);

        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("id"), Some("test-id"));

        let pubsub = iq.get_child("pubsub", NS_PUBSUB);
        assert!(pubsub.is_some());

        let publish = pubsub.unwrap().get_child("publish", NS_PUBSUB);
        assert!(publish.is_some());
        assert_eq!(publish.unwrap().attr("node"), Some(NS_MOOD));

        let item = publish.unwrap().get_child("item", NS_PUBSUB);
        assert!(item.is_some());
        assert_eq!(item.unwrap().attr("id"), Some("current"));
    }
}
