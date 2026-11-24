//! The community feed (XEP-0472): what a post is, in what order they read,
//! what a person may post, and how a post's age and kind are worded.
//!
//! No GTK here. `ui/feed.rs` draws it and `session_feed.rs` fetches it.

use chrono::DateTime;
use url::Url;
use waddle_xmpp_client_ffi::{WaddleFeedEntry, WaddleFeedSource};

use crate::profile;

/// How many posts and stories one refresh asks for, as on the phones.
pub const PAGE_SIZE: u32 = 50;
/// Longer links are refused, as browsers and servers commonly do.
pub const LINK_LIMIT: usize = 2048;

/// The community service of an account's server (`community.<domain>`),
/// where the feed and stories nodes live.
pub fn community_service(domain: &str) -> String {
    format!("community.{domain}")
}

/// What a post the server wrote for a member reports. Posts members wrote
/// have none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Mood,
    Activity,
    Tune,
    Avatar,
    Profile,
    EventReply,
    Other,
}

impl Source {
    /// The label shown above the text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mood => "Mood update",
            Self::Activity => "Activity update",
            Self::Tune => "Listening to",
            Self::Avatar => "New picture",
            Self::Profile => "Profile update",
            Self::EventReply => "Event reply",
            Self::Other => "Update",
        }
    }
}

impl From<WaddleFeedSource> for Source {
    fn from(source: WaddleFeedSource) -> Self {
        match source {
            WaddleFeedSource::Mood => Self::Mood,
            WaddleFeedSource::Activity => Self::Activity,
            WaddleFeedSource::Tune => Self::Tune,
            WaddleFeedSource::Avatar => Self::Avatar,
            WaddleFeedSource::Profile => Self::Profile,
            WaddleFeedSource::EventReply => Self::EventReply,
            WaddleFeedSource::Other => Self::Other,
        }
    }
}

/// One post of the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub id: String,
    /// The author's bare JID, as the server stamped it.
    pub author: Option<String>,
    pub title: Option<String>,
    pub body: String,
    /// Only an address that is safe to open from here.
    pub link: Option<Url>,
    pub published_ms: Option<i64>,
    pub source: Option<Source>,
}

impl Post {
    pub fn from_wire(entry: WaddleFeedEntry) -> Self {
        Self {
            id: entry.id,
            author: entry.author.filter(|author| !author.trim().is_empty()),
            title: entry.title.filter(|title| !title.trim().is_empty()),
            body: entry.body,
            link: entry.link.as_deref().and_then(safe_link),
            published_ms: entry.published_ms,
            source: entry.source.map(Source::from),
        }
    }
}

/// `text` as an address that may be opened: `http` or `https`, with a
/// host and without a user name or password (a post is written by someone
/// else; `https://bank.example@evil.example` must not look like the bank).
pub fn safe_link(text: &str) -> Option<Url> {
    let url = profile::openable(text)?;
    (url.username().is_empty() && url.password().is_none()).then_some(url)
}

/// Posts newest first. The server lists them oldest first and `published`
/// is optional: posts without a time keep the server's order, newest last,
/// and read as oldest.
pub fn newest_first(entries: Vec<WaddleFeedEntry>) -> Vec<Post> {
    let mut posts: Vec<(usize, Post)> = entries
        .into_iter()
        .map(Post::from_wire)
        .enumerate()
        .collect();
    posts.sort_by(|(first_at, first), (second_at, second)| {
        let (a, b) = (first.published_ms, second.published_ms);
        if a == b {
            second_at.cmp(first_at)
        } else {
            b.unwrap_or(i64::MIN).cmp(&a.unwrap_or(i64::MIN))
        }
    });
    posts.into_iter().map(|(_, post)| post).collect()
}

/// Put a post this device just made first, unless the list has it.
pub fn prepend(posts: &mut Vec<Post>, post: Post) {
    posts.retain(|existing| existing.id != post.id);
    posts.insert(0, post);
}

/// What was typed for a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkInput {
    None,
    Web(Url),
    Invalid,
}

/// An http(s) address with a dotted host and no user name or password,
/// with `https://` assumed when no scheme is typed. Stricter than a link
/// in a message: `me@example.com` must not become `https://me@example.com`.
pub fn link_from(text: &str) -> LinkInput {
    let text = text.trim();
    if text.is_empty() {
        return LinkInput::None;
    }
    if text.len() > LINK_LIMIT || text.chars().any(char::is_whitespace) {
        return LinkInput::Invalid;
    }
    let full = if text.contains("://") {
        text.to_string()
    } else {
        format!("https://{text}")
    };
    let Some(url) = safe_link(&full) else {
        return LinkInput::Invalid;
    };
    match url.host_str() {
        Some(host) if host.contains('.') && !host.starts_with('.') && !host.ends_with('.') => {
            LinkInput::Web(url)
        }
        _ => LinkInput::Invalid,
    }
}

/// Why a post cannot be sent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftError {
    Empty,
    BadLink,
    /// A post carries one link, which is also where an image goes.
    LinkAndImage,
}

impl DraftError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Write something to post.",
            Self::BadLink => "The link must be a web address, such as https://waddle.social.",
            Self::LinkAndImage => {
                "A post carries one link, and a picture is shared as one. Remove the link or the picture."
            }
        }
    }
}

/// A post ready to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostDraft {
    pub body: String,
    pub link: Option<String>,
}

impl PostDraft {
    /// `image_url` is a picture already uploaded for this post. The feed
    /// entry has no media of its own (XEP-0472 carries text and a link),
    /// so the picture travels as the post's link and is shown inline.
    pub fn new(body: &str, link_text: &str, image_url: Option<&str>) -> Result<Self, DraftError> {
        let body = body.trim();
        if body.is_empty() {
            return Err(DraftError::Empty);
        }
        let link = match (link_from(link_text), image_url) {
            (LinkInput::Invalid, _) => return Err(DraftError::BadLink),
            (LinkInput::Web(_), Some(_)) => return Err(DraftError::LinkAndImage),
            (LinkInput::Web(url), None) => Some(url.to_string()),
            (LinkInput::None, Some(image)) => Some(image.to_string()),
            (LinkInput::None, None) => None,
        };
        Ok(Self {
            body: body.to_string(),
            link,
        })
    }
}

/// How long ago, as a short phrase. A time in the future reads "just now".
pub fn ago(now_ms: i64, then_ms: i64) -> String {
    let seconds = now_ms.saturating_sub(then_ms).max(0) / 1000;
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", seconds / 60),
        3600..=86_399 => format!("{} h ago", seconds / 3600),
        86_400..=604_799 => format!("{} d ago", seconds / 86_400),
        _ => DateTime::from_timestamp_millis(then_ms)
            .map(|at| at.format("%-d %b %Y").to_string())
            .unwrap_or_default(),
    }
}

/// The name to show for an author: the profile's name once known,
/// otherwise the part of the address before the `@`.
pub fn author_name(author: &str, known: Option<&str>) -> String {
    known
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| author.split('@').next().unwrap_or(author).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, published_ms: Option<i64>) -> WaddleFeedEntry {
        WaddleFeedEntry {
            id: id.to_string(),
            title: None,
            body: format!("body {id}"),
            author: Some("ann@waddle.test".to_string()),
            published_ms,
            link: None,
            source: None,
        }
    }

    fn ids(posts: &[Post]) -> Vec<&str> {
        posts.iter().map(|post| post.id.as_str()).collect()
    }

    #[test]
    fn the_community_service_is_a_subdomain() {
        assert_eq!(community_service("waddle.test"), "community.waddle.test");
    }

    #[test]
    fn posts_read_newest_first() {
        let posts = newest_first(vec![
            entry("old", Some(1_000)),
            entry("new", Some(3_000)),
            entry("mid", Some(2_000)),
        ]);
        assert_eq!(ids(&posts), ["new", "mid", "old"]);
    }

    #[test]
    fn undated_posts_keep_the_servers_order_and_read_oldest() {
        let posts = newest_first(vec![
            entry("a", None),
            entry("b", None),
            entry("dated", Some(5)),
        ]);
        assert_eq!(ids(&posts), ["dated", "b", "a"]);
    }

    #[test]
    fn equal_times_put_the_later_arrival_first() {
        let posts = newest_first(vec![entry("a", Some(7)), entry("b", Some(7))]);
        assert_eq!(ids(&posts), ["b", "a"]);
    }

    #[test]
    fn a_post_made_here_goes_first_once() {
        let mut posts = newest_first(vec![entry("a", Some(1)), entry("b", Some(2))]);
        prepend(&mut posts, Post::from_wire(entry("a", Some(9))));
        assert_eq!(ids(&posts), ["a", "b"]);
        assert_eq!(posts[0].published_ms, Some(9));
    }

    #[test]
    fn only_web_links_without_credentials_may_be_opened() {
        assert!(safe_link("https://waddle.social/x").is_some());
        assert!(safe_link("http://waddle.social").is_some());
        assert!(safe_link("javascript:alert(1)").is_none());
        assert!(safe_link("file:///etc/passwd").is_none());
        assert!(safe_link("mailto:a@b.test").is_none());
        assert!(safe_link("https://bank.example@evil.example/").is_none());
        assert!(safe_link("https://user:pw@example.com/").is_none());
        assert!(safe_link("not a url").is_none());
    }

    #[test]
    fn a_posts_unsafe_link_is_dropped() {
        let mut wire = entry("a", Some(1));
        wire.link = Some("javascript:alert(1)".to_string());
        assert_eq!(Post::from_wire(wire).link, None);
        let mut wire = entry("b", Some(1));
        wire.link = Some("https://waddle.social".to_string());
        assert!(Post::from_wire(wire).link.is_some());
    }

    #[test]
    fn server_written_posts_are_labelled() {
        let mut wire = entry("a", Some(1));
        wire.source = Some(WaddleFeedSource::Mood);
        assert_eq!(
            Post::from_wire(wire).source.map(Source::label),
            Some("Mood update")
        );
        assert_eq!(Post::from_wire(entry("b", Some(1))).source, None);
    }

    #[test]
    fn typed_links_assume_https() {
        assert_eq!(link_from("  "), LinkInput::None);
        assert_eq!(
            link_from("waddle.social/feed"),
            LinkInput::Web(Url::parse("https://waddle.social/feed").unwrap())
        );
        assert_eq!(
            link_from("http://waddle.social"),
            LinkInput::Web(Url::parse("http://waddle.social").unwrap())
        );
    }

    #[test]
    fn typed_links_that_are_not_web_addresses_are_refused() {
        for bad in [
            "me@example.com",
            "ftp://example.com",
            "javascript:alert(1)",
            "localhost",
            "two words.com",
            "https://.com",
            "https://example.",
            "https://user@example.com",
        ] {
            assert_eq!(link_from(bad), LinkInput::Invalid, "{bad}");
        }
        let long = format!("https://example.com/{}", "a".repeat(LINK_LIMIT));
        assert_eq!(link_from(&long), LinkInput::Invalid);
    }

    #[test]
    fn a_post_needs_text() {
        assert_eq!(PostDraft::new("  \n ", "", None), Err(DraftError::Empty));
        let draft = PostDraft::new("  hello  ", "", None).unwrap();
        assert_eq!(draft.body, "hello");
        assert_eq!(draft.link, None);
    }

    #[test]
    fn a_post_takes_a_link_or_a_picture_but_not_both() {
        assert_eq!(
            PostDraft::new("hi", "waddle.social", None).unwrap().link,
            Some("https://waddle.social/".to_string())
        );
        assert_eq!(
            PostDraft::new("hi", "", Some("https://up.test/a.png"))
                .unwrap()
                .link,
            Some("https://up.test/a.png".to_string())
        );
        assert_eq!(
            PostDraft::new("hi", "waddle.social", Some("https://up.test/a.png")),
            Err(DraftError::LinkAndImage)
        );
        assert_eq!(PostDraft::new("hi", "nope", None), Err(DraftError::BadLink));
    }

    #[test]
    fn ages_read_in_the_largest_unit() {
        let now = 1_000_000_000_000;
        assert_eq!(ago(now, now - 5_000), "just now");
        assert_eq!(ago(now, now + 5_000), "just now");
        assert_eq!(ago(now, now - 5 * 60_000), "5 min ago");
        assert_eq!(ago(now, now - 3 * 3_600_000), "3 h ago");
        assert_eq!(ago(now, now - 2 * 86_400_000), "2 d ago");
        assert_eq!(ago(now, now - 30 * 86_400_000), "10 Aug 2001");
    }

    #[test]
    fn authors_show_a_known_name_or_their_local_part() {
        assert_eq!(author_name("ann@waddle.test", None), "ann");
        assert_eq!(author_name("ann@waddle.test", Some("  ")), "ann");
        assert_eq!(author_name("ann@waddle.test", Some("Ann Lee")), "Ann Lee");
    }
}
