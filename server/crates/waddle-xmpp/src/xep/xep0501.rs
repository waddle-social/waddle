//! XEP-0501: Pubsub Stories
//!
//! Ephemeral stories that auto-expire after a configurable duration
//! (default 24 hours). Like Instagram/Snapchat stories for communities.
//!
//! ## XML Format
//!
//! ```xml
//! <item id='story-123' xmlns='http://jabber.org/protocol/pubsub'>
//!   <story xmlns='urn:xmpp:stories:0'
//!          expires='2024-06-02T12:00:00Z'>
//!     <body>Check out what we're building!</body>
//!     <media-url>https://example.com/photo.jpg</media-url>
//!   </story>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - Share temporary updates/photos
//! - Community "what's happening now" feed
//! - Auto-cleanup of old content

use chrono::{DateTime, Duration, Utc};
use minidom::Element;

/// Namespace for XEP-0501 Stories.
pub const NS_STORIES: &str = "urn:xmpp:stories:0";

/// PubSub node for stories.
pub const PUBSUB_NODE_STORIES: &str = "urn:xmpp:stories:0";

/// Default story expiry: 24 hours.
pub const DEFAULT_EXPIRY_HOURS: i64 = 24;

/// A story entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    /// Story ID.
    pub id: String,
    /// Text content.
    pub body: Option<String>,
    /// Media URL (image/video).
    pub media_url: Option<String>,
    /// Author JID.
    pub author: Option<String>,
    /// When the story was posted.
    pub posted: Option<DateTime<Utc>>,
    /// When the story expires.
    pub expires: Option<DateTime<Utc>>,
}

impl Story {
    /// Create a new story.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            body: None,
            media_url: None,
            author: None,
            posted: Some(Utc::now()),
            expires: Some(Utc::now() + Duration::hours(DEFAULT_EXPIRY_HOURS)),
        }
    }

    /// Set the text body.
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Set a media URL.
    pub fn with_media(mut self, url: impl Into<String>) -> Self {
        self.media_url = Some(url.into());
        self
    }

    /// Set the author.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Set custom expiry duration from now.
    pub fn with_expiry(mut self, duration: Duration) -> Self {
        self.expires = Some(Utc::now() + duration);
        self
    }

    /// Set a specific expiry time.
    pub fn with_expires_at(mut self, at: DateTime<Utc>) -> Self {
        self.expires = Some(at);
        self
    }

    /// Returns `true` if the story has expired.
    pub fn is_expired(&self) -> bool {
        self.expires.is_some_and(|e| e < Utc::now())
    }

    /// Returns `true` if the story is still active.
    pub fn is_active(&self) -> bool {
        !self.is_expired()
    }

    /// Remaining time until expiry, if not yet expired.
    pub fn time_remaining(&self) -> Option<Duration> {
        self.expires
            .filter(|e| *e > Utc::now())
            .map(|e| e.signed_duration_since(Utc::now()))
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a story element.
pub fn is_story_element(elem: &Element) -> bool {
    elem.ns() == NS_STORIES && elem.name() == "story"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a story from a `<story/>` element.
pub fn parse_story(item_id: &str, elem: &Element) -> Option<Story> {
    if !is_story_element(elem) {
        return None;
    }

    let text = |name: &str| -> Option<String> {
        elem.children()
            .find(|c| c.name() == name && c.ns() == NS_STORIES)
            .map(|c| c.text())
            .filter(|t| !t.is_empty())
    };

    let expires = elem
        .attr("expires")
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    Some(Story {
        id: item_id.to_owned(),
        body: text("body"),
        media_url: text("media-url"),
        author: text("author"),
        posted: text("posted").and_then(|t| t.parse().ok()),
        expires,
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<story/>` element.
pub fn build_story_element(story: &Story) -> Element {
    let mut builder = Element::builder("story", NS_STORIES);

    if let Some(expires) = story.expires {
        builder = builder.attr("expires", expires.to_rfc3339());
    }

    let mut elem = builder.build();

    let add = |parent: &mut Element, name: &str, value: &str| {
        let mut child = Element::builder(name, NS_STORIES).build();
        child.append_text_node(value);
        parent.append_child(child);
    };

    if let Some(ref body) = story.body {
        add(&mut elem, "body", body);
    }
    if let Some(ref url) = story.media_url {
        add(&mut elem, "media-url", url);
    }
    if let Some(ref author) = story.author {
        add(&mut elem, "author", author);
    }
    if let Some(posted) = story.posted {
        add(&mut elem, "posted", &posted.to_rfc3339());
    }

    elem
}

/// Filter a list of stories to only active (non-expired) ones.
pub fn filter_active(stories: &[Story]) -> Vec<&Story> {
    stories.iter().filter(|s| s.is_active()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn future_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0)
            .single()
            .expect("valid")
    }

    fn past_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn test_is_story_element() {
        let elem = Element::builder("story", NS_STORIES).build();
        assert!(is_story_element(&elem));

        let wrong = Element::builder("story", "jabber:client").build();
        assert!(!is_story_element(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let story = Story::new("s-1")
            .with_body("Hello!")
            .with_media("https://example.com/photo.jpg")
            .with_author("alice@example.com")
            .with_expires_at(future_time());

        let elem = build_story_element(&story);
        assert_eq!(elem.name(), "story");
        assert!(elem.attr("expires").is_some());

        let parsed = parse_story("s-1", &elem).expect("parseable");
        assert_eq!(parsed.id, "s-1");
        assert_eq!(parsed.body.as_deref(), Some("Hello!"));
        assert_eq!(
            parsed.media_url.as_deref(),
            Some("https://example.com/photo.jpg")
        );
        assert_eq!(parsed.author.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.expires, Some(future_time()));
    }

    #[test]
    fn test_parse_minimal() {
        let xml = "<story xmlns='urn:xmpp:stories:0'>\
                    <body>Quick update</body>\
                    </story>";
        let elem: Element = xml.parse().expect("valid");
        let story = parse_story("s-2", &elem).expect("parseable");
        assert_eq!(story.body.as_deref(), Some("Quick update"));
        assert_eq!(story.media_url, None);
    }

    #[test]
    fn test_is_expired() {
        let expired = Story::new("s").with_expires_at(past_time());
        assert!(expired.is_expired());
        assert!(!expired.is_active());

        let active = Story::new("s").with_expires_at(future_time());
        assert!(!active.is_expired());
        assert!(active.is_active());
    }

    #[test]
    fn test_time_remaining() {
        let active = Story::new("s").with_expires_at(future_time());
        assert!(active.time_remaining().is_some());

        let expired = Story::new("s").with_expires_at(past_time());
        assert!(expired.time_remaining().is_none());
    }

    #[test]
    fn test_filter_active() {
        let active = Story::new("a").with_expires_at(future_time());
        let expired = Story::new("e").with_expires_at(past_time());
        let stories = vec![active, expired];

        let filtered = filter_active(&stories);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
    }

    #[test]
    fn test_story_new_defaults() {
        let story = Story::new("s-1");
        assert!(story.posted.is_some());
        assert!(story.expires.is_some());
        assert!(story.is_active());
    }

    #[test]
    fn test_pubsub_node() {
        assert_eq!(PUBSUB_NODE_STORIES, "urn:xmpp:stories:0");
    }
}
