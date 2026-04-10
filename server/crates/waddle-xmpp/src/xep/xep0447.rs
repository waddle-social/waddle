//! XEP-0447: Stateless File Sharing
//!
//! Provides structured file sharing using file metadata (XEP-0446)
//! and download sources. Replaces ad-hoc URL sharing with a proper
//! protocol for sharing files with metadata.
//!
//! ## XML Format
//!
//! ```xml
//! <message type='groupchat' to='room@muc.example.com'>
//!   <body>https://files.example.com/photo.png</body>
//!   <file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'>
//!     <file xmlns='urn:xmpp:file:metadata:0'>
//!       <media-type>image/png</media-type>
//!       <name>photo.png</name>
//!       <size>12345</size>
//!     </file>
//!     <sources>
//!       <url-data xmlns='http://jabber.org/protocol/url-data'
//!                 target='https://files.example.com/photo.png'/>
//!     </sources>
//!   </file-sharing>
//! </message>
//! ```
//!
//! ## Use Cases
//!
//! - Share images with inline preview metadata
//! - Share files with name, size, and type info
//! - Multiple download sources (HTTP, P2P)
//! - Integrates with XEP-0363 HTTP File Upload

use minidom::Element;
use xmpp_parsers::message::Message;

use super::xep0446::{self, FileMetadata};

/// Namespace for XEP-0447 Stateless File Sharing.
pub const NS_SFS: &str = "urn:xmpp:sfs:0";

/// Namespace for URL data sources.
pub const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";

/// Content disposition for shared files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Disposition {
    /// File should be displayed inline (images, videos).
    Inline,
    /// File should be offered as download (documents, archives).
    Attachment,
}

impl Disposition {
    /// Parse from attribute value.
    pub fn from_str_attr(s: &str) -> Option<Self> {
        match s {
            "inline" => Some(Self::Inline),
            "attachment" => Some(Self::Attachment),
            _ => None,
        }
    }

    /// Convert to attribute string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Attachment => "attachment",
        }
    }
}

impl Default for Disposition {
    fn default() -> Self {
        Self::Inline
    }
}

/// A download source for a shared file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// HTTP/HTTPS URL source.
    Url(String),
}

impl Source {
    /// Create an HTTP URL source.
    pub fn url(url: impl Into<String>) -> Self {
        Self::Url(url.into())
    }

    /// Get the URL if this is a URL source.
    pub fn as_url(&self) -> Option<&str> {
        match self {
            Self::Url(u) => Some(u),
        }
    }
}

/// A complete file sharing element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSharing {
    /// The file metadata.
    pub metadata: FileMetadata,
    /// Download sources.
    pub sources: Vec<Source>,
    /// Content disposition.
    pub disposition: Disposition,
}

impl FileSharing {
    /// Create a new file sharing element.
    pub fn new(metadata: FileMetadata) -> Self {
        Self {
            metadata,
            sources: Vec::new(),
            disposition: Disposition::default(),
        }
    }

    /// Add a URL source.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.sources.push(Source::url(url));
        self
    }

    /// Set the disposition.
    pub fn with_disposition(mut self, disposition: Disposition) -> Self {
        self.disposition = disposition;
        self
    }

    /// Get the first URL source, if any.
    pub fn first_url(&self) -> Option<&str> {
        self.sources.iter().find_map(|s| s.as_url())
    }

    /// Returns `true` if inline disposition (images, videos).
    pub fn is_inline(&self) -> bool {
        self.disposition == Disposition::Inline
    }
}

/// Trait for types that can carry file sharing elements.
pub trait FileSharingCarrier {
    /// Extract file sharing info from this carrier.
    fn file_sharing(&self) -> Option<FileSharing>;

    /// Returns `true` if this carrier has a file sharing element.
    fn has_file_sharing(&self) -> bool {
        self.file_sharing().is_some()
    }
}

impl FileSharingCarrier for Message {
    fn file_sharing(&self) -> Option<FileSharing> {
        extract_file_sharing_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<file-sharing/>` element.
pub fn is_file_sharing_element(elem: &Element) -> bool {
    elem.ns() == NS_SFS && elem.name() == "file-sharing"
}

/// Check if a message has file sharing.
pub fn has_file_sharing(msg: &Message) -> bool {
    msg.payloads.iter().any(|e| is_file_sharing_element(e))
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract file sharing from a message.
pub fn extract_file_sharing_from_message(msg: &Message) -> Option<FileSharing> {
    msg.payloads
        .iter()
        .find(|e| is_file_sharing_element(e))
        .and_then(|e| parse_file_sharing_element(e))
}

/// Parse a `<file-sharing/>` element.
pub fn parse_file_sharing_element(elem: &Element) -> Option<FileSharing> {
    if !is_file_sharing_element(elem) {
        return None;
    }

    let disposition = elem
        .attr("disposition")
        .and_then(Disposition::from_str_attr)
        .unwrap_or_default();

    // Parse file metadata
    let file_elem = elem
        .children()
        .find(|c| xep0446::is_file_metadata_element(c))?;
    let metadata = xep0446::parse_file_metadata_element(file_elem);

    // Parse sources
    let mut sources = Vec::new();
    if let Some(sources_elem) = elem.children().find(|c| c.name() == "sources") {
        for child in sources_elem.children() {
            if child.name() == "url-data" && child.ns() == NS_URL_DATA {
                if let Some(target) = child.attr("target").filter(|t| !t.is_empty()) {
                    sources.push(Source::url(target));
                }
            }
        }
    }

    Some(FileSharing {
        metadata,
        sources,
        disposition,
    })
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<file-sharing/>` element.
pub fn build_file_sharing_element(sharing: &FileSharing) -> Element {
    let mut fs = Element::builder("file-sharing", NS_SFS)
        .attr("disposition", sharing.disposition.as_str())
        .build();

    fs.append_child(xep0446::build_file_metadata_element(&sharing.metadata));

    if !sharing.sources.is_empty() {
        let mut sources = Element::builder("sources", NS_SFS).build();
        for source in &sharing.sources {
            match source {
                Source::Url(url) => {
                    let url_data = Element::builder("url-data", NS_URL_DATA)
                        .attr("target", url.as_str())
                        .build();
                    sources.append_child(url_data);
                }
            }
        }
        fs.append_child(sources);
    }

    fs
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add file sharing to a message.
pub fn set_file_sharing(msg: &mut Message, sharing: &FileSharing) {
    msg.payloads.retain(|e| e.ns() != NS_SFS);
    msg.payloads.push(build_file_sharing_element(sharing));
}

/// Remove file sharing from a message.
pub fn strip_file_sharing(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_SFS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    fn test_sharing() -> FileSharing {
        FileSharing::new(
            FileMetadata::new()
                .with_media_type("image/png")
                .with_name("photo.png")
                .with_size(12345),
        )
        .with_url("https://files.example.com/photo.png")
        .with_disposition(Disposition::Inline)
    }

    #[test]
    fn test_is_file_sharing_element() {
        let elem = Element::builder("file-sharing", NS_SFS).build();
        assert!(is_file_sharing_element(&elem));

        let wrong = Element::builder("file-sharing", "jabber:client").build();
        assert!(!is_file_sharing_element(&wrong));
    }

    #[test]
    fn test_build_and_parse() {
        let sharing = test_sharing();
        let elem = build_file_sharing_element(&sharing);

        assert_eq!(elem.name(), "file-sharing");
        assert_eq!(elem.ns(), NS_SFS);
        assert_eq!(elem.attr("disposition"), Some("inline"));

        let parsed = parse_file_sharing_element(&elem).expect("parseable");
        assert_eq!(parsed.metadata.name.as_deref(), Some("photo.png"));
        assert_eq!(parsed.metadata.media_type.as_deref(), Some("image/png"));
        assert_eq!(parsed.metadata.size, Some(12345));
        assert_eq!(parsed.sources.len(), 1);
        assert_eq!(
            parsed.first_url(),
            Some("https://files.example.com/photo.png")
        );
        assert!(parsed.is_inline());
    }

    #[test]
    fn test_parse_from_xml() {
        let xml = "<message xmlns='jabber:client' type='groupchat'>\
                    <file-sharing xmlns='urn:xmpp:sfs:0' disposition='attachment'>\
                      <file xmlns='urn:xmpp:file:metadata:0'>\
                        <name>doc.pdf</name>\
                        <size>999</size>\
                        <media-type>application/pdf</media-type>\
                      </file>\
                      <sources>\
                        <url-data xmlns='http://jabber.org/protocol/url-data' target='https://example.com/doc.pdf'/>\
                      </sources>\
                    </file-sharing>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let sharing = extract_file_sharing_from_message(&msg).expect("has sharing");
        assert_eq!(sharing.metadata.name.as_deref(), Some("doc.pdf"));
        assert_eq!(sharing.disposition, Disposition::Attachment);
        assert!(!sharing.is_inline());
        assert_eq!(
            sharing.first_url(),
            Some("https://example.com/doc.pdf")
        );
    }

    #[test]
    fn test_parse_no_sources() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <file-sharing xmlns='urn:xmpp:sfs:0'>\
                      <file xmlns='urn:xmpp:file:metadata:0'>\
                        <name>file.txt</name>\
                      </file>\
                    </file-sharing>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let sharing = extract_file_sharing_from_message(&msg).expect("has sharing");
        assert!(sharing.sources.is_empty());
        assert_eq!(sharing.first_url(), None);
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_file_sharing_from_message(&msg).is_none());
    }

    #[test]
    fn test_multiple_sources() {
        let sharing = FileSharing::new(FileMetadata::new().with_name("file.zip"))
            .with_url("https://cdn1.example.com/file.zip")
            .with_url("https://cdn2.example.com/file.zip");

        let elem = build_file_sharing_element(&sharing);
        let parsed = parse_file_sharing_element(&elem).expect("parseable");
        assert_eq!(parsed.sources.len(), 2);
    }

    #[test]
    fn test_set_file_sharing() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_file_sharing(&mut msg, &test_sharing());

        assert!(has_file_sharing(&msg));
        let sharing = extract_file_sharing_from_message(&msg).expect("has sharing");
        assert_eq!(sharing.metadata.name.as_deref(), Some("photo.png"));

        // Replace
        let sharing2 = FileSharing::new(FileMetadata::new().with_name("new.jpg"));
        set_file_sharing(&mut msg, &sharing2);
        let extracted = extract_file_sharing_from_message(&msg).expect("has sharing");
        assert_eq!(extracted.metadata.name.as_deref(), Some("new.jpg"));
        assert_eq!(
            msg.payloads.iter().filter(|e| e.ns() == NS_SFS).count(),
            1
        );
    }

    #[test]
    fn test_strip_file_sharing() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_file_sharing(&mut msg, &test_sharing());
        strip_file_sharing(&mut msg);
        assert!(!has_file_sharing(&msg));
    }

    #[test]
    fn test_file_sharing_carrier_trait() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_file_sharing(&mut msg, &test_sharing());

        assert!(msg.has_file_sharing());
        let sharing = msg.file_sharing().expect("has sharing");
        assert!(sharing.metadata.is_image());
    }

    #[test]
    fn test_disposition_default() {
        assert_eq!(Disposition::default(), Disposition::Inline);
    }

    #[test]
    fn test_disposition_parsing() {
        assert_eq!(
            Disposition::from_str_attr("inline"),
            Some(Disposition::Inline)
        );
        assert_eq!(
            Disposition::from_str_attr("attachment"),
            Some(Disposition::Attachment)
        );
        assert_eq!(Disposition::from_str_attr("unknown"), None);
    }

    #[test]
    fn test_source_helpers() {
        let src = Source::url("https://example.com/file");
        assert_eq!(src.as_url(), Some("https://example.com/file"));
    }

    #[test]
    fn test_file_sharing_builder() {
        let sharing = FileSharing::new(FileMetadata::new().with_name("test.txt"))
            .with_url("https://example.com/test.txt")
            .with_disposition(Disposition::Attachment);

        assert_eq!(sharing.disposition, Disposition::Attachment);
        assert_eq!(sharing.sources.len(), 1);
        assert!(!sharing.is_inline());
    }
}
