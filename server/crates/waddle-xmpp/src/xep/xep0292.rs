//! XEP-0292: vCard4 Over XMPP
//!
//! Provides modern user profile data using the vCard4 format (RFC 6350).
//! Replaces legacy vcard-temp (XEP-0054) with structured, extensible profiles.
//!
//! ## XML Format
//!
//! Published via PEP (XEP-0163):
//! ```xml
//! <item id='current' xmlns='http://jabber.org/protocol/pubsub'>
//!   <vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>
//!     <fn><text>Romeo Montague</text></fn>
//!     <nickname><text>Romeo</text></nickname>
//!     <email><text>romeo@example.com</text></email>
//!     <note><text>Star-crossed lover</text></note>
//!   </vcard>
//! </item>
//! ```
//!
//! ## PEP Node
//!
//! Published to: `urn:xmpp:vcard4`
//!
//! ## Use Cases
//!
//! - User profile display (name, avatar, bio)
//! - Contact information sharing
//! - Rich presence cards in chat

use minidom::Element;
use thiserror::Error;

/// Namespace for vCard4.
pub const NS_VCARD4: &str = "urn:ietf:params:xml:ns:vcard-4.0";

/// PEP node for vCard4 publication.
pub const PEP_NODE_VCARD4: &str = "urn:xmpp:vcard4";

/// Errors that can occur when parsing vCard4.
#[derive(Debug, Error)]
pub enum VCard4Error {
    /// Missing required field.
    #[error("missing vCard4 field: {0}")]
    MissingField(String),
}

/// A vCard4 profile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VCard4 {
    /// Full formatted name (FN property).
    pub full_name: Option<String>,
    /// Nickname.
    pub nickname: Option<String>,
    /// Email address.
    pub email: Option<String>,
    /// Phone number.
    pub tel: Option<String>,
    /// Note/bio/about text.
    pub note: Option<String>,
    /// Organization name.
    pub org: Option<String>,
    /// Title/role.
    pub title: Option<String>,
    /// URL (website, social link).
    pub url: Option<String>,
    /// Photo URL or base64 data URI.
    pub photo_uri: Option<String>,
}

impl VCard4 {
    /// Create an empty vCard4.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the full name.
    pub fn with_full_name(mut self, name: impl Into<String>) -> Self {
        self.full_name = Some(name.into());
        self
    }

    /// Set the nickname.
    pub fn with_nickname(mut self, nick: impl Into<String>) -> Self {
        self.nickname = Some(nick.into());
        self
    }

    /// Set the email.
    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    /// Set the note/bio.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Set the organization.
    pub fn with_org(mut self, org: impl Into<String>) -> Self {
        self.org = Some(org.into());
        self
    }

    /// Set the title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the URL.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the photo URI.
    pub fn with_photo(mut self, uri: impl Into<String>) -> Self {
        self.photo_uri = Some(uri.into());
        self
    }

    /// Returns the best display name (full_name > nickname > None).
    pub fn display_name(&self) -> Option<&str> {
        self.full_name
            .as_deref()
            .or(self.nickname.as_deref())
    }

    /// Returns `true` if the vCard has no data.
    pub fn is_empty(&self) -> bool {
        self.full_name.is_none()
            && self.nickname.is_none()
            && self.email.is_none()
            && self.tel.is_none()
            && self.note.is_none()
            && self.org.is_none()
            && self.title.is_none()
            && self.url.is_none()
            && self.photo_uri.is_none()
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a vCard4 element.
pub fn is_vcard4_element(elem: &Element) -> bool {
    elem.ns() == NS_VCARD4 && elem.name() == "vcard"
}

// ── Extraction ───────────────────────────────────────────────────────

/// Parse a vCard4 from a `<vcard/>` element.
pub fn parse_vcard4(elem: &Element) -> VCard4 {
    let text_prop = |name: &str| -> Option<String> {
        elem.children()
            .find(|c| c.name() == name && c.ns() == NS_VCARD4)
            .and_then(|c| c.children().find(|t| t.name() == "text"))
            .map(|t| t.text())
            .filter(|t| !t.is_empty())
    };

    let uri_prop = |name: &str| -> Option<String> {
        elem.children()
            .find(|c| c.name() == name && c.ns() == NS_VCARD4)
            .and_then(|c| {
                c.children()
                    .find(|t| t.name() == "uri" || t.name() == "text")
            })
            .map(|t| t.text())
            .filter(|t| !t.is_empty())
    };

    VCard4 {
        full_name: text_prop("fn"),
        nickname: text_prop("nickname"),
        email: text_prop("email"),
        tel: text_prop("tel"),
        note: text_prop("note"),
        org: text_prop("org"),
        title: text_prop("title"),
        url: uri_prop("url"),
        photo_uri: uri_prop("photo"),
    }
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>` element.
pub fn build_vcard4_element(vcard: &VCard4) -> Element {
    let mut elem = Element::builder("vcard", NS_VCARD4).build();

    let append_text_prop = |parent: &mut Element, name: &str, value: &str| {
        let mut prop = Element::builder(name, NS_VCARD4).build();
        let mut text = Element::builder("text", NS_VCARD4).build();
        text.append_text_node(value);
        prop.append_child(text);
        parent.append_child(prop);
    };

    let append_uri_prop = |parent: &mut Element, name: &str, value: &str| {
        let mut prop = Element::builder(name, NS_VCARD4).build();
        let mut uri = Element::builder("uri", NS_VCARD4).build();
        uri.append_text_node(value);
        prop.append_child(uri);
        parent.append_child(prop);
    };

    if let Some(ref v) = vcard.full_name {
        append_text_prop(&mut elem, "fn", v);
    }
    if let Some(ref v) = vcard.nickname {
        append_text_prop(&mut elem, "nickname", v);
    }
    if let Some(ref v) = vcard.email {
        append_text_prop(&mut elem, "email", v);
    }
    if let Some(ref v) = vcard.tel {
        append_text_prop(&mut elem, "tel", v);
    }
    if let Some(ref v) = vcard.note {
        append_text_prop(&mut elem, "note", v);
    }
    if let Some(ref v) = vcard.org {
        append_text_prop(&mut elem, "org", v);
    }
    if let Some(ref v) = vcard.title {
        append_text_prop(&mut elem, "title", v);
    }
    if let Some(ref v) = vcard.url {
        append_uri_prop(&mut elem, "url", v);
    }
    if let Some(ref v) = vcard.photo_uri {
        append_uri_prop(&mut elem, "photo", v);
    }

    elem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_vcard4_element() {
        let elem = Element::builder("vcard", NS_VCARD4).build();
        assert!(is_vcard4_element(&elem));

        let wrong = Element::builder("vcard", "vcard-temp").build();
        assert!(!is_vcard4_element(&wrong));
    }

    #[test]
    fn test_parse_full_vcard() {
        let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                    <fn><text>Romeo Montague</text></fn>\
                    <nickname><text>Romeo</text></nickname>\
                    <email><text>romeo@example.com</text></email>\
                    <note><text>Star-crossed lover</text></note>\
                    <org><text>Montague Inc</text></org>\
                    <title><text>Heir</text></title>\
                    <url><uri>https://romeo.example.com</uri></url>\
                    <photo><uri>https://example.com/romeo.jpg</uri></photo>\
                    </vcard>";
        let elem: Element = xml.parse().expect("valid xml");
        let vcard = parse_vcard4(&elem);

        assert_eq!(vcard.full_name.as_deref(), Some("Romeo Montague"));
        assert_eq!(vcard.nickname.as_deref(), Some("Romeo"));
        assert_eq!(vcard.email.as_deref(), Some("romeo@example.com"));
        assert_eq!(vcard.note.as_deref(), Some("Star-crossed lover"));
        assert_eq!(vcard.org.as_deref(), Some("Montague Inc"));
        assert_eq!(vcard.title.as_deref(), Some("Heir"));
        assert_eq!(vcard.url.as_deref(), Some("https://romeo.example.com"));
        assert_eq!(vcard.photo_uri.as_deref(), Some("https://example.com/romeo.jpg"));
        assert!(!vcard.is_empty());
    }

    #[test]
    fn test_parse_minimal_vcard() {
        let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'>\
                    <fn><text>Alice</text></fn>\
                    </vcard>";
        let elem: Element = xml.parse().expect("valid xml");
        let vcard = parse_vcard4(&elem);

        assert_eq!(vcard.full_name.as_deref(), Some("Alice"));
        assert_eq!(vcard.nickname, None);
        assert_eq!(vcard.email, None);
    }

    #[test]
    fn test_parse_empty_vcard() {
        let xml = "<vcard xmlns='urn:ietf:params:xml:ns:vcard-4.0'/>";
        let elem: Element = xml.parse().expect("valid xml");
        let vcard = parse_vcard4(&elem);
        assert!(vcard.is_empty());
    }

    #[test]
    fn test_build_vcard4() {
        let vcard = VCard4::new()
            .with_full_name("Juliet Capulet")
            .with_nickname("Juliet")
            .with_email("juliet@example.com")
            .with_note("Also star-crossed")
            .with_url("https://juliet.example.com")
            .with_photo("https://example.com/juliet.jpg");

        let elem = build_vcard4_element(&vcard);
        assert_eq!(elem.name(), "vcard");
        assert_eq!(elem.ns(), NS_VCARD4);

        // Roundtrip
        let parsed = parse_vcard4(&elem);
        assert_eq!(parsed.full_name.as_deref(), Some("Juliet Capulet"));
        assert_eq!(parsed.nickname.as_deref(), Some("Juliet"));
        assert_eq!(parsed.email.as_deref(), Some("juliet@example.com"));
        assert_eq!(parsed.note.as_deref(), Some("Also star-crossed"));
        assert_eq!(parsed.url.as_deref(), Some("https://juliet.example.com"));
        assert_eq!(parsed.photo_uri.as_deref(), Some("https://example.com/juliet.jpg"));
    }

    #[test]
    fn test_build_empty_vcard() {
        let vcard = VCard4::new();
        let elem = build_vcard4_element(&vcard);
        assert_eq!(elem.children().count(), 0);
    }

    #[test]
    fn test_display_name() {
        let both = VCard4::new()
            .with_full_name("Full Name")
            .with_nickname("Nick");
        assert_eq!(both.display_name(), Some("Full Name"));

        let nick_only = VCard4::new().with_nickname("Nick");
        assert_eq!(nick_only.display_name(), Some("Nick"));

        let empty = VCard4::new();
        assert_eq!(empty.display_name(), None);
    }

    #[test]
    fn test_vcard4_builder() {
        let v = VCard4::new()
            .with_full_name("Test")
            .with_org("Org")
            .with_title("Dev");
        assert_eq!(v.full_name.as_deref(), Some("Test"));
        assert_eq!(v.org.as_deref(), Some("Org"));
        assert_eq!(v.title.as_deref(), Some("Dev"));
    }

    #[test]
    fn test_vcard4_default() {
        let v = VCard4::default();
        assert!(v.is_empty());
        assert_eq!(v.display_name(), None);
    }

    #[test]
    fn test_pep_node() {
        assert_eq!(PEP_NODE_VCARD4, "urn:xmpp:vcard4");
    }
}
