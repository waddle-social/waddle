//! XEP-0402: PEP Native Bookmarks
//!
//! Implements MUC room bookmarks stored using PEP (Personal Eventing Protocol).
//!
//! ## Overview
//!
//! PEP Native Bookmarks supersedes XEP-0048 (Bookmarks) and XEP-0049
//! (Private XML Storage) for storing MUC room bookmarks. It stores each
//! bookmark as a separate PEP item, keyed by the room's JID.
//!
//! ## Key Features
//!
//! - Bookmarks stored in PEP node `urn:xmpp:bookmarks:1`
//! - Each bookmark is a separate item with `id` = room JID
//! - Supports autojoin, nick, password, and extensions
//! - Access model should be `whitelist` (private to user)
//!
//! ## XML Format
//!
//! ```xml
//! <!-- Publishing a bookmark -->
//! <iq type='set' id='pub1'>
//!   <pubsub xmlns='http://jabber.org/protocol/pubsub'>
//!     <publish node='urn:xmpp:bookmarks:1'>
//!       <item id='theplay@conference.shakespeare.lit'>
//!         <conference xmlns='urn:xmpp:bookmarks:1'
//!                     name='The Play&apos;s the Thing'
//!                     autojoin='true'>
//!           <nick>JC</nick>
//!         </conference>
//!       </item>
//!     </publish>
//!   </pubsub>
//! </iq>
//!
//! <!-- Retrieved bookmark -->
//! <message from='juliet@capulet.lit' to='juliet@capulet.lit/balcony' type='headline'>
//!   <event xmlns='http://jabber.org/protocol/pubsub#event'>
//!     <items node='urn:xmpp:bookmarks:1'>
//!       <item id='theplay@conference.shakespeare.lit'>
//!         <conference xmlns='urn:xmpp:bookmarks:1'
//!                     name='The Play&apos;s the Thing'
//!                     autojoin='true'>
//!           <nick>JC</nick>
//!         </conference>
//!       </item>
//!     </items>
//!   </event>
//! </message>
//! ```

use jid::BareJid;
use minidom::Element;

/// Namespace for XEP-0402 PEP Native Bookmarks.
pub const NS_BOOKMARKS2: &str = "urn:xmpp:bookmarks:1";

/// PEP node name for bookmarks.
pub const PEP_NODE: &str = "urn:xmpp:bookmarks:1";

/// A MUC room bookmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// JID of the MUC room.
    pub jid: BareJid,
    /// Optional human-readable name for the room.
    pub name: Option<String>,
    /// Whether to automatically join the room on login.
    pub autojoin: bool,
    /// Preferred nickname to use in the room.
    pub nick: Option<String>,
    /// Optional password for password-protected rooms.
    pub password: Option<String>,
    /// Optional extensions (unrecognized child elements).
    pub extensions: Vec<Element>,
}

impl Default for Bookmark {
    fn default() -> Self {
        Self {
            jid: "room@conference.example.com".parse().expect("default jid"),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: Vec::new(),
        }
    }
}

impl Bookmark {
    /// Create a new bookmark for a room.
    pub fn new(jid: BareJid) -> Self {
        Self {
            jid,
            ..Default::default()
        }
    }

    /// Create a bookmark with autojoin enabled.
    pub fn with_autojoin(mut self, autojoin: bool) -> Self {
        self.autojoin = autojoin;
        self
    }

    /// Set the room name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the preferred nickname.
    pub fn with_nick(mut self, nick: impl Into<String>) -> Self {
        self.nick = Some(nick.into());
        self
    }

    /// Set the room password.
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }
}

/// Errors that can occur when parsing bookmarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkError {
    /// Missing required element or attribute.
    MissingElement(String),
    /// Invalid JID format.
    InvalidJid(String),
    /// Wrong namespace.
    WrongNamespace(String),
    /// Wrong element name.
    WrongElement(String),
}

impl std::fmt::Display for BookmarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BookmarkError::MissingElement(s) => write!(f, "Missing element: {}", s),
            BookmarkError::InvalidJid(s) => write!(f, "Invalid JID: {}", s),
            BookmarkError::WrongNamespace(s) => write!(f, "Wrong namespace: {}", s),
            BookmarkError::WrongElement(s) => write!(f, "Wrong element: {}", s),
        }
    }
}

impl std::error::Error for BookmarkError {}

/// Parse a bookmark from a PubSub item payload.
///
/// Expects a `<conference xmlns='urn:xmpp:bookmarks:1'>` element.
/// The room JID comes from the item ID, not from within the conference element.
pub fn parse_bookmark(item_id: &str, payload: &Element) -> Result<Bookmark, BookmarkError> {
    // Validate element name and namespace
    if payload.name() != "conference" {
        return Err(BookmarkError::WrongElement(format!(
            "Expected 'conference', got '{}'",
            payload.name()
        )));
    }

    if payload.ns() != NS_BOOKMARKS2 {
        return Err(BookmarkError::WrongNamespace(format!(
            "Expected '{}', got '{}'",
            NS_BOOKMARKS2,
            payload.ns()
        )));
    }

    // Parse room JID from item ID
    let jid: BareJid = item_id
        .parse()
        .map_err(|e| BookmarkError::InvalidJid(format!("{}: {}", item_id, e)))?;

    // Parse attributes
    let name = payload.attr("name").map(String::from);
    let autojoin = payload
        .attr("autojoin")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    // Parse child elements
    let nick = payload.get_child("nick", NS_BOOKMARKS2).map(|e| e.text());
    let password = payload
        .get_child("password", NS_BOOKMARKS2)
        .map(|e| e.text());

    // Collect unrecognized extensions
    let extensions: Vec<Element> = payload
        .children()
        .filter(|c| {
            let name = c.name();
            name != "nick" && name != "password" && name != "extensions"
        })
        .cloned()
        .collect();

    Ok(Bookmark {
        jid,
        name,
        autojoin,
        nick,
        password,
        extensions,
    })
}

/// Build a bookmark conference element for publishing.
///
/// Returns a `<conference xmlns='urn:xmpp:bookmarks:1'>` element.
pub fn build_bookmark_element(bookmark: &Bookmark) -> Element {
    let mut builder = Element::builder("conference", NS_BOOKMARKS2);

    if let Some(ref name) = bookmark.name {
        builder = builder.attr("name", name);
    }

    if bookmark.autojoin {
        builder = builder.attr("autojoin", "true");
    }

    if let Some(ref nick) = bookmark.nick {
        builder = builder.append(
            Element::builder("nick", NS_BOOKMARKS2)
                .append(nick.clone())
                .build(),
        );
    }

    if let Some(ref password) = bookmark.password {
        builder = builder.append(
            Element::builder("password", NS_BOOKMARKS2)
                .append(password.clone())
                .build(),
        );
    }

    // Add any extensions
    for ext in &bookmark.extensions {
        builder = builder.append(ext.clone());
    }

    builder.build()
}

/// Build a PubSub item for a bookmark.
///
/// The item ID is the room's bare JID.
pub fn build_bookmark_item(bookmark: &Bookmark) -> crate::pubsub::PubSubItem {
    let payload = build_bookmark_element(bookmark);
    crate::pubsub::PubSubItem::new(Some(bookmark.jid.to_string()), Some(payload))
}

/// Check if a PubSub node name is the bookmarks node.
pub fn is_bookmarks_node(node: &str) -> bool {
    node == PEP_NODE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bookmark_minimal() {
        let elem: Element = r#"<conference xmlns='urn:xmpp:bookmarks:1'/>"#
            .parse()
            .expect("valid xml");

        let bookmark = parse_bookmark("room@conference.example.com", &elem).expect("should parse");

        assert_eq!(bookmark.jid.to_string(), "room@conference.example.com");
        assert_eq!(bookmark.name, None);
        assert!(!bookmark.autojoin);
        assert_eq!(bookmark.nick, None);
        assert_eq!(bookmark.password, None);
    }

    #[test]
    fn test_parse_bookmark_full() {
        let elem: Element =
            r#"<conference xmlns='urn:xmpp:bookmarks:1' name='The Room' autojoin='true'>
            <nick>MyNick</nick>
            <password>secret</password>
        </conference>"#
                .parse()
                .expect("valid xml");

        let bookmark = parse_bookmark("room@conference.example.com", &elem).expect("should parse");

        assert_eq!(bookmark.jid.to_string(), "room@conference.example.com");
        assert_eq!(bookmark.name, Some("The Room".to_string()));
        assert!(bookmark.autojoin);
        assert_eq!(bookmark.nick, Some("MyNick".to_string()));
        assert_eq!(bookmark.password, Some("secret".to_string()));
    }

    #[test]
    fn test_parse_bookmark_wrong_namespace() {
        let elem: Element = r#"<conference xmlns='wrong:ns'/>"#.parse().expect("valid xml");

        let result = parse_bookmark("room@conference.example.com", &elem);
        assert!(matches!(result, Err(BookmarkError::WrongNamespace(_))));
    }

    #[test]
    fn test_parse_bookmark_wrong_element() {
        let elem: Element = r#"<notconference xmlns='urn:xmpp:bookmarks:1'/>"#
            .parse()
            .expect("valid xml");

        let result = parse_bookmark("room@conference.example.com", &elem);
        assert!(matches!(result, Err(BookmarkError::WrongElement(_))));
    }

    #[test]
    fn test_parse_bookmark_invalid_jid() {
        let elem: Element = r#"<conference xmlns='urn:xmpp:bookmarks:1'/>"#
            .parse()
            .expect("valid xml");

        // Use an actually invalid JID format (empty string or clearly invalid)
        let result = parse_bookmark("", &elem);
        assert!(matches!(result, Err(BookmarkError::InvalidJid(_))));
    }

    #[test]
    fn test_build_bookmark_element() {
        let jid: BareJid = "room@conference.example.com".parse().expect("valid jid");
        let bookmark = Bookmark::new(jid)
            .with_name("Test Room")
            .with_autojoin(true)
            .with_nick("TestNick");

        let elem = build_bookmark_element(&bookmark);

        assert_eq!(elem.name(), "conference");
        assert_eq!(elem.ns(), NS_BOOKMARKS2);
        assert_eq!(elem.attr("name"), Some("Test Room"));
        assert_eq!(elem.attr("autojoin"), Some("true"));

        let nick = elem.get_child("nick", NS_BOOKMARKS2).expect("nick element");
        assert_eq!(nick.text(), "TestNick");
    }

    #[test]
    fn test_build_bookmark_item() {
        let jid: BareJid = "room@conference.example.com".parse().expect("valid jid");
        let bookmark = Bookmark::new(jid);

        let item = build_bookmark_item(&bookmark);

        assert_eq!(item.id, Some("room@conference.example.com".to_string()));
        assert!(item.payload.is_some());
    }

    #[test]
    fn test_bookmark_round_trip() {
        let jid: BareJid = "room@conference.example.com".parse().expect("valid jid");
        let original = Bookmark::new(jid.clone())
            .with_name("Test Room")
            .with_autojoin(true)
            .with_nick("TestNick")
            .with_password("secret123");

        let elem = build_bookmark_element(&original);
        let parsed = parse_bookmark(&jid.to_string(), &elem).expect("should parse");

        assert_eq!(parsed.jid, original.jid);
        assert_eq!(parsed.name, original.name);
        assert_eq!(parsed.autojoin, original.autojoin);
        assert_eq!(parsed.nick, original.nick);
        assert_eq!(parsed.password, original.password);
    }

    #[test]
    fn test_is_bookmarks_node() {
        assert!(is_bookmarks_node("urn:xmpp:bookmarks:1"));
        assert!(!is_bookmarks_node("some:other:node"));
    }
}
