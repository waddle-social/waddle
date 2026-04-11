//! XEP-0449: Stickers
//!
//! Provides sticker pack metadata and sticker message elements. Stickers
//! are images organized into packs, shared via file metadata (XEP-0446)
//! and stateless file sharing (XEP-0447).
//!
//! ## XML Format
//!
//! A sticker message:
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <body>:thumbsup:</body>
//!   <sticker xmlns='urn:xmpp:stickers:0' pack='https://example.com/stickers/pack1'/>
//!   <file-sharing xmlns='urn:xmpp:sfs:0'>
//!     <file xmlns='urn:xmpp:file:metadata:0'>
//!       <media-type>image/webp</media-type>
//!       <name>thumbsup.webp</name>
//!       <size>4096</size>
//!     </file>
//!     <sources>
//!       <url-data xmlns='http://jabber.org/protocol/url-data'
//!                 target='https://example.com/stickers/thumbsup.webp'/>
//!     </sources>
//!   </file-sharing>
//! </message>
//! ```
//!
//! ## Sticker Pack (PubSub)
//!
//! Packs are published to a PubSub node with items containing sticker metadata.
//!
//! ## Use Cases
//!
//! - Browse and select stickers from packs
//! - Send sticker messages (image + fallback text)
//! - Custom community sticker packs

use minidom::Element;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0449 Stickers.
pub const NS_STICKERS: &str = "urn:xmpp:stickers:0";

/// A sticker reference in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerRef {
    /// The sticker pack URI/ID.
    pub pack: String,
}

impl StickerRef {
    /// Create a new sticker reference.
    pub fn new(pack: impl Into<String>) -> Self {
        Self { pack: pack.into() }
    }
}

/// Metadata for a single sticker within a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sticker {
    /// Suggested text (e.g., `:thumbsup:` or emoji).
    pub suggest: String,
    /// The file URL for the sticker image.
    pub url: Option<String>,
    /// MIME type of the sticker image.
    pub media_type: Option<String>,
    /// Content hash for deduplication/caching.
    pub hash: Option<String>,
}

impl Sticker {
    /// Create a new sticker with suggested text.
    pub fn new(suggest: impl Into<String>) -> Self {
        Self {
            suggest: suggest.into(),
            url: None,
            media_type: None,
            hash: None,
        }
    }

    /// Set the URL.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the media type.
    pub fn with_media_type(mut self, mt: impl Into<String>) -> Self {
        self.media_type = Some(mt.into());
        self
    }

    /// Set the content hash.
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.hash = Some(hash.into());
        self
    }
}

/// A sticker pack containing multiple stickers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerPack {
    /// Pack name/title.
    pub name: String,
    /// Pack URI/identifier.
    pub id: String,
    /// Stickers in this pack.
    pub stickers: Vec<Sticker>,
}

impl StickerPack {
    /// Create a new sticker pack.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
            stickers: Vec::new(),
        }
    }

    /// Add a sticker to the pack.
    pub fn with_sticker(mut self, sticker: Sticker) -> Self {
        self.stickers.push(sticker);
        self
    }

    /// Number of stickers in the pack.
    pub fn len(&self) -> usize {
        self.stickers.len()
    }

    /// Returns `true` if the pack has no stickers.
    pub fn is_empty(&self) -> bool {
        self.stickers.is_empty()
    }
}

/// Trait for types that can carry sticker references.
pub trait StickerCarrier {
    /// Extract the sticker reference from this carrier.
    fn sticker_ref(&self) -> Option<StickerRef>;

    /// Returns `true` if this carrier is a sticker message.
    fn is_sticker(&self) -> bool {
        self.sticker_ref().is_some()
    }
}

impl StickerCarrier for Message {
    fn sticker_ref(&self) -> Option<StickerRef> {
        extract_sticker_ref(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<sticker/>` element.
pub fn is_sticker_element(elem: &Element) -> bool {
    elem.ns() == NS_STICKERS && elem.name() == "sticker"
}

/// Check if a message is a sticker message.
pub fn is_sticker_message(msg: &Message) -> bool {
    msg.payloads.iter().any(|e| is_sticker_element(e))
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract a sticker reference from a message.
pub fn extract_sticker_ref(msg: &Message) -> Option<StickerRef> {
    let elem = msg.payloads.iter().find(|e| is_sticker_element(e))?;
    let pack = elem.attr("pack").filter(|s| !s.is_empty())?.to_owned();
    Some(StickerRef::new(pack))
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<sticker xmlns='urn:xmpp:stickers:0' pack='...'/>` element.
pub fn build_sticker_element(sticker_ref: &StickerRef) -> Element {
    Element::builder("sticker", NS_STICKERS)
        .attr("pack", sticker_ref.pack.as_str())
        .build()
}

/// Build a sticker message (body fallback + sticker ref + file sharing).
///
/// The body serves as fallback text for clients that don't support stickers.
pub fn build_sticker_message(
    to: impl Into<Option<jid::Jid>>,
    sticker_ref: &StickerRef,
    fallback_text: &str,
    image_url: &str,
    media_type: &str,
) -> Message {
    use xmpp_parsers::message::Body;

    let mut msg = Message::new(to.into());
    msg.type_ = xmpp_parsers::message::MessageType::Groupchat;
    msg.id = Some(uuid::Uuid::new_v4().to_string());
    msg.bodies
        .insert(String::new(), Body(fallback_text.to_owned()));
    msg.payloads.push(build_sticker_element(sticker_ref));

    // Add file-sharing element
    let file_sharing = super::xep0447::FileSharing::new(
        super::xep0446::FileMetadata::new()
            .with_media_type(media_type)
            .with_name(fallback_text),
    )
    .with_url(image_url)
    .with_disposition(super::xep0447::Disposition::Inline);
    msg.payloads
        .push(super::xep0447::build_file_sharing_element(&file_sharing));

    msg
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add a sticker reference to a message.
pub fn set_sticker_ref(msg: &mut Message, sticker_ref: &StickerRef) {
    msg.payloads.retain(|e| e.ns() != NS_STICKERS);
    msg.payloads.push(build_sticker_element(sticker_ref));
}

/// Remove sticker reference from a message.
pub fn strip_sticker_ref(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_STICKERS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::MessageType;

    #[test]
    fn test_is_sticker_element() {
        let elem = Element::builder("sticker", NS_STICKERS)
            .attr("pack", "pack-1")
            .build();
        assert!(is_sticker_element(&elem));

        let wrong = Element::builder("sticker", "jabber:client").build();
        assert!(!is_sticker_element(&wrong));
    }

    #[test]
    fn test_extract_sticker_ref() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <body>:thumbsup:</body>\
                    <sticker xmlns='urn:xmpp:stickers:0' pack='https://example.com/pack1'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let sref = extract_sticker_ref(&msg).expect("has sticker");
        assert_eq!(sref.pack, "https://example.com/pack1");
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_sticker_ref(&msg).is_none());
    }

    #[test]
    fn test_build_sticker_element() {
        let sref = StickerRef::new("pack-123");
        let elem = build_sticker_element(&sref);

        assert_eq!(elem.name(), "sticker");
        assert_eq!(elem.ns(), NS_STICKERS);
        assert_eq!(elem.attr("pack"), Some("pack-123"));
    }

    #[test]
    fn test_build_sticker_message() {
        let to: jid::Jid = "room@muc.example.com".parse().expect("valid jid");
        let sref = StickerRef::new("https://example.com/pack1");
        let msg = build_sticker_message(
            to.clone(),
            &sref,
            ":thumbsup:",
            "https://example.com/thumbsup.webp",
            "image/webp",
        );

        assert_eq!(msg.to, Some(to));
        assert_eq!(msg.type_, MessageType::Groupchat);
        assert!(is_sticker_message(&msg));
        assert!(!msg.bodies.is_empty()); // Has fallback body
                                         // Has file-sharing element too
        assert!(msg
            .payloads
            .iter()
            .any(|e| e.ns() == super::super::xep0447::NS_SFS));
    }

    #[test]
    fn test_set_sticker_ref() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_sticker_ref(&mut msg, &StickerRef::new("pack-1"));
        assert!(is_sticker_message(&msg));

        // Replace
        set_sticker_ref(&mut msg, &StickerRef::new("pack-2"));
        let sref = extract_sticker_ref(&msg).expect("has sticker");
        assert_eq!(sref.pack, "pack-2");
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_STICKERS)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_sticker_ref() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_sticker_ref(&mut msg, &StickerRef::new("pack-1"));
        strip_sticker_ref(&mut msg);
        assert!(!is_sticker_message(&msg));
    }

    #[test]
    fn test_sticker_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <sticker xmlns='urn:xmpp:stickers:0' pack='pack-test'/>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.is_sticker());
        assert_eq!(
            msg.sticker_ref().map(|s| s.pack),
            Some("pack-test".to_owned())
        );
    }

    #[test]
    fn test_sticker_pack_builder() {
        let pack = StickerPack::new("pack-1", "Fun Stickers")
            .with_sticker(
                Sticker::new(":wave:")
                    .with_url("https://example.com/wave.webp")
                    .with_media_type("image/webp"),
            )
            .with_sticker(Sticker::new(":heart:").with_hash("abc123"));

        assert_eq!(pack.name, "Fun Stickers");
        assert_eq!(pack.id, "pack-1");
        assert_eq!(pack.len(), 2);
        assert!(!pack.is_empty());
        assert_eq!(pack.stickers[0].suggest, ":wave:");
        assert_eq!(
            pack.stickers[0].url.as_deref(),
            Some("https://example.com/wave.webp")
        );
        assert_eq!(pack.stickers[1].hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_sticker_ref_new() {
        let sref = StickerRef::new("my-pack");
        assert_eq!(sref.pack, "my-pack");
    }

    #[test]
    fn test_empty_pack() {
        let pack = StickerPack::new("empty", "Empty Pack");
        assert!(pack.is_empty());
        assert_eq!(pack.len(), 0);
    }
}
