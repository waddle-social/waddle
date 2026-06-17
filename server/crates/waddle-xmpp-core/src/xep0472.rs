//! XEP-0472: Pubsub Social Feed
//!
//! Social feed posts via PubSub. Enables community announcements,
//! microblogging, and activity feeds within XMPP.
//!
//! ## XML Format
//!
//! A social post published to PubSub:
//! ```xml
//! <item id='post-123' xmlns='http://jabber.org/protocol/pubsub'>
//!   <entry xmlns='urn:xmpp:pubsub-social-feed:0'>
//!     <title>Announcement</title>
//!     <body>We're launching a new feature!</body>
//!     <author>alice@example.com</author>
//!     <published>2024-06-01T12:00:00Z</published>
//!   </entry>
//! </item>
//! ```
//!
//! ## Use Cases
//!
//! - Community announcements and news
//! - Microblogging / status updates
//! - Activity feed aggregation across spaces

use chrono::{DateTime, Utc};
use minidom::Element;

/// Namespace for XEP-0472 Social Feed.
pub const NS_SOCIAL_FEED: &str = "urn:xmpp:pubsub-social-feed:0";

/// PubSub node for social feed posts.
pub const PUBSUB_NODE_FEED: &str = "urn:xmpp:pubsub-social-feed:0";

/// A social feed post/entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEntry {
    /// Post ID.
    pub id: String,
    /// Optional title.
    pub title: Option<String>,
    /// Post body/content.
    pub body: String,
    /// Author JID or display name.
    pub author: Option<String>,
    /// Publication timestamp.
    pub published: Option<DateTime<Utc>>,
    /// Optional link/URL.
    pub link: Option<String>,
}

impl FeedEntry {
    /// Create a new feed entry.
    pub fn new(id: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: None,
            body: body.into(),
            author: None,
            published: None,
            link: None,
        }
    }

    /// Set the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the author.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Set publication time to now.
    pub fn with_published_now(mut self) -> Self {
        self.published = Some(Utc::now());
        self
    }

    /// Set a specific publication time.
    pub fn with_published(mut self, ts: DateTime<Utc>) -> Self {
        self.published = Some(ts);
        self
    }

    /// Set a link.
    pub fn with_link(mut self, link: impl Into<String>) -> Self {
        self.link = Some(link.into());
        self
    }

    /// A short preview of the body (first N chars).
    pub fn preview(&self, max_len: usize) -> &str {
        if self.body.len() <= max_len {
            &self.body
        } else {
            let end = self
                .body
                .char_indices()
                .nth(max_len)
                .map(|(i, _)| i)
                .unwrap_or(self.body.len());
            &self.body[..end]
        }
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a feed entry element.
pub fn is_feed_entry(elem: &Element) -> bool {
    elem.ns() == NS_SOCIAL_FEED && elem.name() == "entry"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a feed entry from an `<entry/>` element.
pub fn parse_feed_entry(item_id: &str, elem: &Element) -> Option<FeedEntry> {
    if !is_feed_entry(elem) {
        return None;
    }

    let text = |name: &str| -> Option<String> {
        elem.children()
            .find(|c| c.name() == name && c.ns() == NS_SOCIAL_FEED)
            .map(|c| c.text())
            .filter(|t| !t.is_empty())
    };

    let body = text("body")?;

    Some(FeedEntry {
        id: item_id.to_owned(),
        title: text("title"),
        body,
        author: text("author"),
        published: text("published").and_then(|t| t.parse().ok()),
        link: text("link"),
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<entry/>` element for PubSub publication.
pub fn build_feed_entry_element(entry: &FeedEntry) -> Element {
    let mut elem = Element::builder("entry", NS_SOCIAL_FEED).build();

    let add = |parent: &mut Element, name: &str, value: &str| {
        let mut child = Element::builder(name, NS_SOCIAL_FEED).build();
        child.append_text_node(value);
        parent.append_child(child);
    };

    if let Some(ref title) = entry.title {
        add(&mut elem, "title", title);
    }
    add(&mut elem, "body", &entry.body);
    if let Some(ref author) = entry.author {
        add(&mut elem, "author", author);
    }
    if let Some(ts) = entry.published {
        add(&mut elem, "published", &ts.to_rfc3339());
    }
    if let Some(ref link) = entry.link {
        add(&mut elem, "link", link);
    }

    elem
}

/// Return a copy of a feed `<entry/>` whose `<author>` is `author`,
/// replacing any author the client supplied. On the open, member-postable
/// feed the service stamps the authenticated publisher here so a member
/// cannot impersonate another user through the displayed payload author.
/// All other children are preserved.
pub fn stamp_feed_entry_author(elem: &Element, author: &str) -> Element {
    let mut stamped = Element::builder("entry", NS_SOCIAL_FEED).build();
    for child in elem.children() {
        if child.name() == "author" && child.ns() == NS_SOCIAL_FEED {
            continue;
        }
        stamped.append_child(child.clone());
    }
    let mut author_el = Element::builder("author", NS_SOCIAL_FEED).build();
    author_el.append_text_node(author);
    stamped.append_child(author_el);
    stamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn test_is_feed_entry() {
        let elem = Element::builder("entry", NS_SOCIAL_FEED).build();
        assert!(is_feed_entry(&elem));

        let wrong = Element::builder("entry", "jabber:client").build();
        assert!(!is_feed_entry(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let entry = FeedEntry::new("p-1", "Hello community!")
            .with_title("Announcement")
            .with_author("alice@example.com")
            .with_published(test_time())
            .with_link("https://example.com/post/1");

        let elem = build_feed_entry_element(&entry);
        assert_eq!(elem.name(), "entry");

        let parsed = parse_feed_entry("p-1", &elem).expect("parseable");
        assert_eq!(parsed.id, "p-1");
        assert_eq!(parsed.body, "Hello community!");
        assert_eq!(parsed.title.as_deref(), Some("Announcement"));
        assert_eq!(parsed.author.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.published, Some(test_time()));
        assert_eq!(parsed.link.as_deref(), Some("https://example.com/post/1"));
    }

    #[test]
    fn test_parse_minimal() {
        let xml = "<entry xmlns='urn:xmpp:pubsub-social-feed:0'>\
                    <body>Just a quick update</body>\
                    </entry>";
        let elem: Element = xml.parse().expect("valid");
        let entry = parse_feed_entry("p-2", &elem).expect("parseable");
        assert_eq!(entry.body, "Just a quick update");
        assert_eq!(entry.title, None);
        assert_eq!(entry.author, None);
    }

    #[test]
    fn test_parse_missing_body() {
        let xml = "<entry xmlns='urn:xmpp:pubsub-social-feed:0'>\
                    <title>No body</title>\
                    </entry>";
        let elem: Element = xml.parse().expect("valid");
        assert!(parse_feed_entry("p-3", &elem).is_none());
    }

    #[test]
    fn test_preview() {
        let entry = FeedEntry::new("p", "Hello world, this is a longer message");
        assert_eq!(entry.preview(5), "Hello");
        assert_eq!(entry.preview(100), "Hello world, this is a longer message");
    }

    #[test]
    fn test_entry_builder() {
        let e = FeedEntry::new("id", "body")
            .with_title("T")
            .with_author("A")
            .with_link("L");
        assert_eq!(e.title.as_deref(), Some("T"));
        assert_eq!(e.author.as_deref(), Some("A"));
        assert_eq!(e.link.as_deref(), Some("L"));
    }

    #[test]
    fn test_with_published_now() {
        let e = FeedEntry::new("id", "body").with_published_now();
        assert!(e.published.is_some());
    }

    #[test]
    fn test_pubsub_node() {
        assert_eq!(PUBSUB_NODE_FEED, "urn:xmpp:pubsub-social-feed:0");
    }

    #[test]
    fn test_stamp_feed_entry_author_overrides_and_preserves() {
        // A client-supplied (spoofed) author is replaced; title/body/link
        // and the entry shape survive.
        let entry = FeedEntry::new("p-9", "Body text")
            .with_title("Title")
            .with_author("admin@example.com")
            .with_published(test_time())
            .with_link("https://example.com/p/9");
        let elem = build_feed_entry_element(&entry);

        let stamped = stamp_feed_entry_author(&elem, "member@example.com");
        assert!(is_feed_entry(&stamped));

        let parsed = parse_feed_entry("p-9", &stamped).expect("parseable");
        assert_eq!(parsed.author.as_deref(), Some("member@example.com"));
        assert_eq!(parsed.body, "Body text");
        assert_eq!(parsed.title.as_deref(), Some("Title"));
        assert_eq!(parsed.published, Some(test_time()));
        assert_eq!(parsed.link.as_deref(), Some("https://example.com/p/9"));

        // Exactly one author element after stamping.
        let authors = stamped
            .children()
            .filter(|c| c.name() == "author" && c.ns() == NS_SOCIAL_FEED)
            .count();
        assert_eq!(authors, 1);
    }

    #[test]
    fn test_stamp_feed_entry_author_adds_when_absent() {
        let elem = build_feed_entry_element(&FeedEntry::new("p-10", "No author here"));
        let stamped = stamp_feed_entry_author(&elem, "member@example.com");
        let parsed = parse_feed_entry("p-10", &stamped).expect("parseable");
        assert_eq!(parsed.author.as_deref(), Some("member@example.com"));
        assert_eq!(parsed.body, "No author here");
    }
}
