//! XEP-0501: Pubsub Stories
//!
//! Ephemeral stories that auto-expire after a configurable duration
//! (default 24 hours). Like Instagram/Snapchat stories for communities.
//!
//! ## XML Format
//!
//! ```xml
//! <item id='story-123' xmlns='http://jabber.org/protocol/pubsub'>
//!   <entry xmlns='http://www.w3.org/2005/Atom'>
//!     <title type='text'>Check out what we're building!</title>
//!     <id>story-123</id>
//!     <published>2024-06-01T12:00:00Z</published>
//!     <updated>2024-06-01T12:00:00Z</updated>
//!     <content type='text'>Check out what we're building!</content>
//!     <link rel='enclosure'
//!           href='https://example.com/photo.jpg'
//!           type='image/jpeg'/>
//!     <expires xmlns='urn:waddle:stories:0'>2024-06-02T12:00:00Z</expires>
//!   </entry>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - Share temporary updates/photos
//! - Community "what's happening now" feed
//! - Auto-cleanup of old content

use std::{fmt, str::FromStr};

use chrono::{DateTime, Duration, Utc};
use minidom::Element;

/// Namespace for XEP-0501 Stories.
pub const NS_STORIES: &str = "urn:xmpp:pubsub-social-feed:stories:0";

/// Atom namespace used for XEP-0501 story item payloads.
pub const NS_ATOM: &str = "http://www.w3.org/2005/Atom";

/// Waddle extension namespace for story fields Atom does not model.
pub const NS_WADDLE_STORIES: &str = "urn:waddle:stories:0";

/// PubSub node for stories.
pub const PUBSUB_NODE_STORIES: &str = "urn:xmpp:pubsub-social-feed:stories:0";

/// Default story expiry: 24 hours.
pub const DEFAULT_EXPIRY_HOURS: i64 = 24;

/// MIME type attached to a story Atom enclosure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryMediaType(String);

impl StoryMediaType {
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let (top, sub) = value.split_once('/')?;
        if top.is_empty()
            || sub.is_empty()
            || value
                .chars()
                .any(|ch| ch.is_ascii_control() || ch.is_whitespace())
        {
            return None;
        }
        Some(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StoryMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for StoryMediaType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

/// A story entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    /// Story ID.
    pub id: String,
    /// Text content.
    pub body: Option<String>,
    /// Media URL (image/video).
    pub media_url: Option<String>,
    /// Media MIME type for the Atom enclosure link.
    pub media_type: Option<StoryMediaType>,
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
            media_type: None,
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

    /// Set the media MIME type.
    pub fn with_media_type(mut self, media_type: StoryMediaType) -> Self {
        self.media_type = Some(media_type);
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
    elem.ns() == NS_ATOM && elem.name() == "entry" && enclosure_links(elem).next().is_some()
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a story from an Atom `<entry/>` element.
pub fn parse_story(item_id: &str, elem: &Element) -> Option<Story> {
    if elem.ns() != NS_ATOM || elem.name() != "entry" {
        return None;
    }

    let _title = atom_child_text(elem, "title")?;
    let _atom_id = atom_child_text(elem, "id")?;
    let updated = atom_child_text(elem, "updated")?.parse().ok()?;
    let mut enclosures = enclosure_links(elem);
    let enclosure = enclosures.next()?;
    if enclosures.next().is_some() {
        return None;
    }
    let media_url = enclosure
        .attr("href")
        .map(str::trim)
        .filter(|href| !href.is_empty())?
        .to_owned();
    let media_type = match enclosure.attr("type") {
        Some(raw) => Some(StoryMediaType::parse(raw)?),
        None => None,
    };
    let posted = if let Some(posted) = atom_child_text(elem, "published") {
        Some(posted.parse().ok()?)
    } else {
        Some(updated)
    };
    let expires = elem
        .children()
        .find(|c| c.name() == "expires" && c.ns() == NS_WADDLE_STORIES)
        .map(|c| c.text())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    Some(Story {
        id: item_id.to_owned(),
        body: atom_child_text(elem, "content").or_else(|| atom_child_text(elem, "summary")),
        media_url: Some(media_url),
        media_type,
        author: atom_author(elem),
        posted,
        expires,
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a story Atom `<entry/>` element.
pub fn build_story_element(story: &Story) -> Element {
    let mut elem = Element::builder("entry", NS_ATOM).build();
    let posted = story.posted.unwrap_or_else(Utc::now).to_rfc3339();
    let title = story
        .body
        .as_deref()
        .filter(|body| !body.trim().is_empty())
        .unwrap_or("Story");

    append_atom_text(&mut elem, "title", title, Some("text"));
    append_atom_text(&mut elem, "id", &story.id, None);
    append_atom_text(&mut elem, "published", &posted, None);
    append_atom_text(&mut elem, "updated", &posted, None);
    if let Some(ref body) = story.body {
        append_atom_text(&mut elem, "content", body, Some("text"));
    }
    if let Some(url) = story
        .media_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        elem.append_child(build_atom_link(
            url,
            Some("enclosure"),
            story.media_type.as_ref().map(StoryMediaType::as_str),
        ));
    }
    if let Some(ref author) = story.author {
        elem.append_child(build_atom_author(author));
    }
    if let Some(expires) = story.expires {
        let mut expires_el = Element::builder("expires", NS_WADDLE_STORIES).build();
        expires_el.append_text_node(expires.to_rfc3339());
        elem.append_child(expires_el);
    }

    elem
}

/// Stamp a story payload with the authenticated publisher as author.
pub fn stamp_story_author(item_id: &str, elem: &Element, author: &str) -> Option<Element> {
    parse_story(item_id, elem)?;

    let mut stamped = elem.clone();
    let author_children: Vec<(String, String)> = stamped
        .children()
        .filter(|child| child.name() == "author" && child.ns() == NS_ATOM)
        .map(|child| (child.name().to_owned(), child.ns()))
        .collect();
    for (name, ns) in author_children {
        stamped.remove_child(&name, ns.as_str());
    }

    stamped.append_child(build_atom_author(author));

    Some(stamped)
}

/// Filter a list of stories to only active (non-expired) ones.
pub fn filter_active(stories: &[Story]) -> Vec<&Story> {
    stories.iter().filter(|s| s.is_active()).collect()
}

fn enclosure_links(elem: &Element) -> impl Iterator<Item = &Element> {
    elem.children()
        .filter(|c| c.name() == "link" && c.ns() == NS_ATOM && c.attr("rel") == Some("enclosure"))
}

fn atom_child_text(elem: &Element, name: &str) -> Option<String> {
    elem.children()
        .find(|c| c.name() == name && c.ns() == NS_ATOM)
        .map(|c| c.text())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn atom_author(elem: &Element) -> Option<String> {
    let author = elem
        .children()
        .find(|c| c.name() == "author" && c.ns() == NS_ATOM)?;
    atom_child_text(author, "uri")
        .map(|uri| uri.strip_prefix("xmpp:").unwrap_or(&uri).to_owned())
        .or_else(|| atom_child_text(author, "name"))
        .or_else(|| {
            let text = author.text().trim().to_owned();
            (!text.is_empty()).then_some(text)
        })
}

fn append_atom_text(parent: &mut Element, name: &str, value: &str, type_attr: Option<&str>) {
    let mut builder = Element::builder(name, NS_ATOM);
    if let Some(type_attr) = type_attr {
        builder = builder.attr(minidom::rxml::xml_ncname!("type").to_owned(), type_attr);
    }
    let mut child = builder.build();
    child.append_text_node(value);
    parent.append_child(child);
}

fn build_atom_author(author: &str) -> Element {
    let mut uri = Element::builder("uri", NS_ATOM).build();
    uri.append_text_node(format!("xmpp:{author}"));
    Element::builder("author", NS_ATOM).append(uri).build()
}

fn build_atom_link(href: &str, rel: Option<&str>, media_type: Option<&str>) -> Element {
    let mut builder =
        Element::builder("link", NS_ATOM).attr(minidom::rxml::xml_ncname!("href").to_owned(), href);
    if let Some(rel) = rel {
        builder = builder.attr(minidom::rxml::xml_ncname!("rel").to_owned(), rel);
    }
    if let Some(media_type) = media_type {
        builder = builder.attr(minidom::rxml::xml_ncname!("type").to_owned(), media_type);
    }
    builder.build()
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
        let elem: Element =
            "<entry xmlns='http://www.w3.org/2005/Atom'><link rel='enclosure' href='https://example.com/photo.jpg'/></entry>"
                .parse()
                .expect("atom story");
        assert!(is_story_element(&elem));

        let wrong = Element::builder("entry", NS_ATOM).build();
        assert!(!is_story_element(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let story = Story::new("s-1")
            .with_body("Hello!")
            .with_media("https://example.com/photo.jpg")
            .with_media_type("image/jpeg".parse().expect("media type"))
            .with_author("alice@example.com")
            .with_expires_at(future_time());

        let elem = build_story_element(&story);
        assert_eq!(elem.name(), "entry");
        assert_eq!(elem.ns(), NS_ATOM);

        let parsed = parse_story("s-1", &elem).expect("parseable");
        assert_eq!(parsed.id, "s-1");
        assert_eq!(parsed.body.as_deref(), Some("Hello!"));
        assert_eq!(
            parsed.media_url.as_deref(),
            Some("https://example.com/photo.jpg")
        );
        assert_eq!(
            parsed.media_type.as_ref().map(StoryMediaType::as_str),
            Some("image/jpeg")
        );
        assert_eq!(parsed.author.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.expires, Some(future_time()));
    }

    #[test]
    fn test_stamp_story_author_replaces_payload_author() {
        let story = Story::new("s-1")
            .with_body("Hello!")
            .with_media("https://example.com/photo.jpg")
            .with_author("spoof@example.com")
            .with_expires_at(future_time());
        let elem = build_story_element(&story);

        let stamped = stamp_story_author("s-1", &elem, "alice@example.com").expect("story");
        let parsed = parse_story("s-1", &stamped).expect("parseable");

        assert_eq!(parsed.author.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.body.as_deref(), Some("Hello!"));
        assert_eq!(
            parsed.media_url.as_deref(),
            Some("https://example.com/photo.jpg")
        );
        assert_eq!(parsed.expires, story.expires);
    }

    #[test]
    fn test_stamp_story_author_preserves_expires_wire_value() {
        let elem: Element =
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Hello!</title><id>s-1</id><updated>2026-06-01T12:00:00Z</updated><content>Hello!</content><author><uri>xmpp:spoof@example.com</uri></author><link rel='enclosure' href='https://example.com/photo.jpg' type='image/jpeg'/><expires xmlns='urn:waddle:stories:0'>2030-01-01T12:00:00Z</expires></entry>"
                .parse()
                .expect("story element");

        let stamped = stamp_story_author("s-1", &elem, "alice@example.com").expect("story");

        assert_eq!(
            stamped
                .get_child("expires", NS_WADDLE_STORIES)
                .map(|expires| expires.text())
                .as_deref(),
            Some("2030-01-01T12:00:00Z")
        );
        assert_eq!(
            parse_story("s-1", &stamped)
                .expect("parseable")
                .author
                .as_deref(),
            Some("alice@example.com")
        );
    }

    #[test]
    fn test_parse_minimal() {
        let xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                    <title type='text'>Quick update</title>\
                    <id>s-2</id>\
                    <updated>2026-06-01T12:00:00Z</updated>\
                    <content type='text'>Quick update</content>\
                    <link rel='enclosure' href='https://example.com/photo.jpg'/>\
                    </entry>";
        let elem: Element = xml.parse().expect("valid");
        let story = parse_story("s-2", &elem).expect("parseable");
        assert_eq!(story.body.as_deref(), Some("Quick update"));
        assert_eq!(
            story.media_url.as_deref(),
            Some("https://example.com/photo.jpg")
        );
    }

    #[test]
    fn test_parse_missing_required_atom_fields() {
        for xml in [
            "<entry xmlns='http://www.w3.org/2005/Atom'><id>s</id><updated>2026-06-01T12:00:00Z</updated><link rel='enclosure' href='https://example.com/photo.jpg'/></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Story</title><updated>2026-06-01T12:00:00Z</updated><link rel='enclosure' href='https://example.com/photo.jpg'/></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Story</title><id>s</id><link rel='enclosure' href='https://example.com/photo.jpg'/></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Story</title><id>s</id><updated>not-a-date</updated><link rel='enclosure' href='https://example.com/photo.jpg'/></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Story</title><id>s</id><updated>2026-06-01T12:00:00Z</updated><link rel='enclosure' href='   '/></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Story</title><id>s</id><updated>2026-06-01T12:00:00Z</updated><published>not-a-date</published><link rel='enclosure' href='https://example.com/photo.jpg'/></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Story</title><id>s</id><updated>2026-06-01T12:00:00Z</updated><link rel='enclosure' href='https://example.com/photo.jpg' type='not-a-mime'/></entry>",
        ] {
            let elem: Element = xml.parse().expect("valid");
            assert!(parse_story("s-required", &elem).is_none());
        }
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
        assert_eq!(PUBSUB_NODE_STORIES, "urn:xmpp:pubsub-social-feed:stories:0");
    }

    #[test]
    fn test_is_story_element_negative() {
        let wrong = Element::builder("other", NS_ATOM).build();
        assert!(!is_story_element(&wrong));
        let wrong_ns = Element::builder("story", "wrong:ns").build();
        assert!(!is_story_element(&wrong_ns));
    }

    #[test]
    fn test_default_expiry_hours() {
        assert_eq!(DEFAULT_EXPIRY_HOURS, 24);
    }

    #[test]
    fn test_story_builder_chain() {
        let s = Story::new("id")
            .with_body("hello")
            .with_media("https://example.com/img.jpg")
            .with_media_type("image/jpeg".parse().expect("media type"))
            .with_author("alice@example.com");
        assert_eq!(s.body.as_deref(), Some("hello"));
        assert_eq!(s.media_url.as_deref(), Some("https://example.com/img.jpg"));
        assert_eq!(
            s.media_type.as_ref().map(StoryMediaType::as_str),
            Some("image/jpeg")
        );
        assert_eq!(s.author.as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn test_filter_active_all_expired() {
        let e1 = Story::new("a").with_expires_at(past_time());
        let e2 = Story::new("b").with_expires_at(past_time());
        assert!(filter_active(&[e1, e2]).is_empty());
    }

    #[test]
    fn test_filter_active_empty() {
        let empty: Vec<Story> = vec![];
        assert!(filter_active(&empty).is_empty());
    }
}
