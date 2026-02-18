//! XEP-0048: Bookmark Storage (Legacy Compatibility)
//!
//! Provides compatibility with the legacy `storage:bookmarks` format
//! by translating to/from the native XEP-0402 bookmark format.
//!
//! Legacy clients may query bookmarks via:
//! - XEP-0049 Private XML Storage (`jabber:iq:private` with `storage:bookmarks`)
//! - PubSub node `storage:bookmarks`
//!
//! This module handles the translation between the legacy format and
//! the native XEP-0402 format used internally.

use jid::BareJid;
use minidom::Element;

use super::xep0402::Bookmark;

/// Namespace for legacy bookmarks (XEP-0048).
pub const NS_BOOKMARKS_LEGACY: &str = "storage:bookmarks";

/// A legacy bookmark in XEP-0048 format.
#[derive(Debug, Clone)]
pub struct LegacyBookmark {
    /// JID of the MUC room.
    pub jid: String,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// Whether to automatically join.
    pub autojoin: bool,
    /// Preferred nickname.
    pub nick: Option<String>,
    /// Optional password.
    pub password: Option<String>,
}

/// Check if a namespace is the legacy bookmarks namespace.
pub fn is_legacy_bookmarks_namespace(ns: &str) -> bool {
    ns == NS_BOOKMARKS_LEGACY
}

/// Convert a native XEP-0402 bookmark to a legacy XEP-0048 format.
pub fn from_native_bookmark(bookmark: &Bookmark) -> LegacyBookmark {
    LegacyBookmark {
        jid: bookmark.jid.to_string(),
        name: bookmark.name.clone(),
        autojoin: bookmark.autojoin,
        nick: bookmark.nick.clone(),
        password: bookmark.password.clone(),
    }
}

/// Convert a legacy XEP-0048 bookmark to native XEP-0402 format.
pub fn to_native_bookmark(legacy: &LegacyBookmark) -> Option<Bookmark> {
    let jid: BareJid = legacy.jid.parse().ok()?;
    let mut bookmark = Bookmark::new(jid);
    bookmark.name = legacy.name.clone();
    bookmark.autojoin = legacy.autojoin;
    bookmark.nick = legacy.nick.clone();
    bookmark.password = legacy.password.clone();
    Some(bookmark)
}

/// Parse legacy bookmarks from a `storage:bookmarks` element.
pub fn parse_legacy_bookmarks(element: &Element) -> Vec<LegacyBookmark> {
    let mut bookmarks = Vec::new();

    for child in element.children() {
        if child.name() == "conference" && child.ns() == NS_BOOKMARKS_LEGACY {
            let jid = match child.attr("jid") {
                Some(j) => j.to_string(),
                None => continue,
            };

            let name = child.attr("name").map(|s| s.to_string());
            let autojoin = child
                .attr("autojoin")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false);

            let nick = child
                .children()
                .find(|c| c.name() == "nick")
                .map(|c| c.text());

            let password = child
                .children()
                .find(|c| c.name() == "password")
                .map(|c| c.text());

            bookmarks.push(LegacyBookmark {
                jid,
                name,
                autojoin,
                nick,
                password,
            });
        }
    }

    bookmarks
}

/// Build a legacy `storage:bookmarks` element from a list of bookmarks.
pub fn build_legacy_bookmarks_element(bookmarks: &[LegacyBookmark]) -> Element {
    let mut storage_builder = Element::builder("storage", NS_BOOKMARKS_LEGACY);

    for bookmark in bookmarks {
        let mut conf_builder = Element::builder("conference", NS_BOOKMARKS_LEGACY)
            .attr("jid", &bookmark.jid)
            .attr("autojoin", if bookmark.autojoin { "true" } else { "false" });

        if let Some(ref name) = bookmark.name {
            conf_builder = conf_builder.attr("name", name);
        }

        if let Some(ref nick) = bookmark.nick {
            let nick_elem = Element::builder("nick", NS_BOOKMARKS_LEGACY)
                .append(minidom::Node::Text(nick.clone()))
                .build();
            conf_builder = conf_builder.append(nick_elem);
        }

        if let Some(ref password) = bookmark.password {
            let password_elem = Element::builder("password", NS_BOOKMARKS_LEGACY)
                .append(minidom::Node::Text(password.clone()))
                .build();
            conf_builder = conf_builder.append(password_elem);
        }

        storage_builder = storage_builder.append(conf_builder.build());
    }

    storage_builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_legacy_bookmarks_namespace() {
        assert!(is_legacy_bookmarks_namespace("storage:bookmarks"));
        assert!(!is_legacy_bookmarks_namespace("urn:xmpp:bookmarks:1"));
    }

    #[test]
    fn test_parse_legacy_bookmarks() {
        let xml = r#"<storage xmlns='storage:bookmarks'>
            <conference jid='room@muc.example.com' name='Test Room' autojoin='true'>
                <nick>TestUser</nick>
            </conference>
        </storage>"#;

        let elem: Element = xml.parse().expect("valid XML");
        let bookmarks = parse_legacy_bookmarks(&elem);

        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].jid, "room@muc.example.com");
        assert_eq!(bookmarks[0].name.as_deref(), Some("Test Room"));
        assert!(bookmarks[0].autojoin);
        assert_eq!(bookmarks[0].nick.as_deref(), Some("TestUser"));
    }

    #[test]
    fn test_roundtrip_conversion() {
        let native = Bookmark::new("room@muc.example.com".parse().expect("valid jid"))
            .with_name("Test Room")
            .with_autojoin(true)
            .with_nick("TestNick");

        let legacy = from_native_bookmark(&native);
        assert_eq!(legacy.jid, "room@muc.example.com");
        assert_eq!(legacy.name.as_deref(), Some("Test Room"));
        assert!(legacy.autojoin);
        assert_eq!(legacy.nick.as_deref(), Some("TestNick"));

        let back = to_native_bookmark(&legacy);
        assert!(back.is_some());
        let back = back.unwrap();
        assert_eq!(back.jid.to_string(), "room@muc.example.com");
        assert_eq!(back.name.as_deref(), Some("Test Room"));
        assert!(back.autojoin);
        assert_eq!(back.nick.as_deref(), Some("TestNick"));
    }

    #[test]
    fn test_build_legacy_bookmarks_element() {
        let bookmarks = vec![LegacyBookmark {
            jid: "room@muc.example.com".to_string(),
            name: Some("Test".to_string()),
            autojoin: true,
            nick: Some("Nick".to_string()),
            password: None,
        }];

        let elem = build_legacy_bookmarks_element(&bookmarks);
        assert_eq!(elem.name(), "storage");
        assert_eq!(elem.ns(), NS_BOOKMARKS_LEGACY);

        let conferences: Vec<_> = elem.children().collect();
        assert_eq!(conferences.len(), 1);
        assert_eq!(conferences[0].attr("jid"), Some("room@muc.example.com"));
    }
}
