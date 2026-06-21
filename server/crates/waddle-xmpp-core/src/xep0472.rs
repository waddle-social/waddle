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
//!   <entry xmlns='http://www.w3.org/2005/Atom'>
//!     <title type='text'>Announcement</title>
//!     <id>tag:example.com,2024:post-123</id>
//!     <updated>2024-06-01T12:00:00Z</updated>
//!     <content type='text'>We're launching a new feature!</content>
//!     <author><uri>xmpp:alice@example.com</uri></author>
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
pub const NS_SOCIAL_FEED: &str = "urn:xmpp:pubsub-social-feed:1";

/// Atom namespace used for XEP-0472 feed item payloads.
pub const NS_ATOM: &str = "http://www.w3.org/2005/Atom";

/// PubSub node for social feed posts.
pub const PUBSUB_NODE_FEED: &str = "urn:xmpp:pubsub-social-feed:1";

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
    elem.ns() == NS_ATOM && elem.name() == "entry"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a feed entry from an `<entry/>` element.
pub fn parse_feed_entry(item_id: &str, elem: &Element) -> Option<FeedEntry> {
    if !is_feed_entry(elem) {
        return None;
    }

    let title = atom_child_text(elem, "title")?;
    let _atom_id = atom_child_text(elem, "id")?;
    let updated = atom_child_text(elem, "updated")?.parse().ok()?;
    let body = atom_child_text(elem, "content")
        .or_else(|| atom_child_text(elem, "summary"))
        .unwrap_or_else(|| title.clone());
    let published = if let Some(published) = atom_child_text(elem, "published") {
        Some(published.parse().ok()?)
    } else {
        Some(updated)
    };

    Some(FeedEntry {
        id: item_id.to_owned(),
        title: Some(title),
        body,
        author: atom_author(elem),
        published,
        link: atom_link_href(elem, None),
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build an `<entry/>` element for PubSub publication.
pub fn build_feed_entry_element(entry: &FeedEntry) -> Element {
    let mut elem = Element::builder("entry", NS_ATOM).build();
    let title = entry
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| {
            let preview = entry.preview(80).trim();
            if preview.is_empty() {
                "Untitled"
            } else {
                preview
            }
        });
    let timestamp = entry.published.unwrap_or_else(Utc::now).to_rfc3339();

    append_atom_text(&mut elem, "title", title, Some("text"));
    append_atom_text(&mut elem, "id", &entry.id, None);
    append_atom_text(&mut elem, "published", &timestamp, None);
    append_atom_text(&mut elem, "updated", &timestamp, None);
    if !entry.body.trim().is_empty() {
        append_atom_text(&mut elem, "content", &entry.body, Some("text"));
    }
    if let Some(ref author) = entry.author {
        elem.append_child(build_atom_author(author));
    }
    if let Some(ref link) = entry.link {
        elem.append_child(build_atom_link(link, Some("alternate"), None));
    }

    elem
}

/// Return a copy of a feed Atom `<entry/>` whose `<author>` is `author`,
/// replacing any author the client supplied. On the open, member-postable
/// feed the service stamps the authenticated publisher here so a member
/// cannot impersonate another user through the displayed payload author.
/// All other children are preserved.
pub fn stamp_feed_entry_author(elem: &Element, author: &str) -> Element {
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
    stamped
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

fn atom_link_href(elem: &Element, rel: Option<&str>) -> Option<String> {
    elem.children()
        .filter(|c| c.name() == "link" && c.ns() == NS_ATOM)
        .find(|link| match rel {
            Some(expected) => match link.attr("rel") {
                Some(actual) => actual == expected,
                None => true,
            },
            None => true,
        })
        .and_then(|link| link.attr("href"))
        .map(str::to_owned)
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

    fn test_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
            .single()
            .expect("valid")
    }

    #[test]
    fn test_is_feed_entry() {
        let elem = Element::builder("entry", NS_ATOM).build();
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
        assert_eq!(elem.ns(), NS_ATOM);

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
        let xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                    <title type='text'>Just a quick update</title>\
                    <id>tag:example.com,2024:p-2</id>\
                    <updated>2024-06-01T12:00:00Z</updated>\
                    </entry>";
        let elem: Element = xml.parse().expect("valid");
        let entry = parse_feed_entry("p-2", &elem).expect("parseable");
        assert_eq!(entry.body, "Just a quick update");
        assert_eq!(entry.title.as_deref(), Some("Just a quick update"));
        assert_eq!(entry.author, None);
    }

    #[test]
    fn test_parse_missing_title() {
        let xml = "<entry xmlns='http://www.w3.org/2005/Atom'>\
                    <id>tag:example.com,2024:p-3</id>\
                    <updated>2024-06-01T12:00:00Z</updated>\
                    <content>No title</content>\
                    </entry>";
        let elem: Element = xml.parse().expect("valid");
        assert!(parse_feed_entry("p-3", &elem).is_none());
    }

    #[test]
    fn test_parse_missing_required_atom_fields() {
        for xml in [
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>No id</title><updated>2024-06-01T12:00:00Z</updated></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>No updated</title><id>tag:example.com,2024:no-updated</id></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Bad updated</title><id>tag:example.com,2024:bad-updated</id><updated>not-a-date</updated></entry>",
            "<entry xmlns='http://www.w3.org/2005/Atom'><title>Bad published</title><id>tag:example.com,2024:bad-published</id><updated>2024-06-01T12:00:00Z</updated><published>not-a-date</published></entry>",
        ] {
            let elem: Element = xml.parse().expect("valid");
            assert!(parse_feed_entry("p-required", &elem).is_none());
        }
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
        assert_eq!(PUBSUB_NODE_FEED, "urn:xmpp:pubsub-social-feed:1");
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
            .filter(|c| c.name() == "author" && c.ns() == NS_ATOM)
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
