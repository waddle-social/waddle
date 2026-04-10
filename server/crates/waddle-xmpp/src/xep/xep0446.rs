//! XEP-0446: File Metadata Element
//!
//! Provides structured file metadata for sharing via XMPP. Used alongside
//! XEP-0363 (HTTP File Upload) and XEP-0447 (Stateless File Sharing) to
//! describe files with name, size, media type, and optional dimensions.
//!
//! ## XML Format
//!
//! ```xml
//! <file xmlns='urn:xmpp:file:metadata:0'>
//!   <media-type>image/png</media-type>
//!   <name>photo.png</name>
//!   <size>12345</size>
//!   <width>800</width>
//!   <height>600</height>
//!   <desc>A sunset photo</desc>
//! </file>
//! ```
//!
//! ## Use Cases
//!
//! - Enrich HTTP File Upload messages with file info
//! - Display file previews (name, size, type) before download
//! - Show image dimensions for layout purposes
//! - Provide file descriptions for accessibility

use minidom::Element;
use thiserror::Error;
use xmpp_parsers::message::Message;

/// Namespace for XEP-0446 File Metadata.
pub const NS_FILE_METADATA: &str = "urn:xmpp:file:metadata:0";

/// Errors that can occur when parsing file metadata.
#[derive(Debug, Error)]
pub enum FileMetadataError {
    /// Invalid size value.
    #[error("invalid file size: {0}")]
    InvalidSize(String),
}

/// Structured metadata about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    /// The MIME media type (e.g., `image/png`, `application/pdf`).
    pub media_type: Option<String>,
    /// The file name.
    pub name: Option<String>,
    /// The file size in bytes.
    pub size: Option<u64>,
    /// Image/video width in pixels.
    pub width: Option<u32>,
    /// Image/video height in pixels.
    pub height: Option<u32>,
    /// Human-readable description.
    pub desc: Option<String>,
    /// Duration in seconds (for audio/video).
    pub duration: Option<u64>,
}

impl FileMetadata {
    /// Create empty file metadata.
    pub fn new() -> Self {
        Self {
            media_type: None,
            name: None,
            size: None,
            width: None,
            height: None,
            desc: None,
            duration: None,
        }
    }

    /// Set the media type.
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    /// Set the file name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the file size in bytes.
    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// Set image/video dimensions.
    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Set the description.
    pub fn with_desc(mut self, desc: impl Into<String>) -> Self {
        self.desc = Some(desc.into());
        self
    }

    /// Set the duration in seconds.
    pub fn with_duration(mut self, duration: u64) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Returns `true` if this is an image based on media type.
    pub fn is_image(&self) -> bool {
        self.media_type
            .as_deref()
            .is_some_and(|mt| mt.starts_with("image/"))
    }

    /// Returns `true` if this is a video based on media type.
    pub fn is_video(&self) -> bool {
        self.media_type
            .as_deref()
            .is_some_and(|mt| mt.starts_with("video/"))
    }

    /// Returns `true` if this is audio based on media type.
    pub fn is_audio(&self) -> bool {
        self.media_type
            .as_deref()
            .is_some_and(|mt| mt.starts_with("audio/"))
    }

    /// Format the file size as a human-readable string.
    pub fn human_size(&self) -> Option<String> {
        self.size.map(|s| {
            if s < 1024 {
                format!("{s} B")
            } else if s < 1024 * 1024 {
                format!("{:.1} KB", s as f64 / 1024.0)
            } else if s < 1024 * 1024 * 1024 {
                format!("{:.1} MB", s as f64 / (1024.0 * 1024.0))
            } else {
                format!("{:.1} GB", s as f64 / (1024.0 * 1024.0 * 1024.0))
            }
        })
    }
}

impl Default for FileMetadata {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait for types that can carry file metadata.
pub trait FileMetadataCarrier {
    /// Extract file metadata from this carrier, if present.
    fn file_metadata(&self) -> Option<FileMetadata>;

    /// Returns `true` if this carrier has file metadata.
    fn has_file_metadata(&self) -> bool {
        self.file_metadata().is_some()
    }
}

impl FileMetadataCarrier for Message {
    fn file_metadata(&self) -> Option<FileMetadata> {
        extract_file_metadata_from_message(self)
    }
}

// ── Detection ────────────────────────────────────────────────────────

/// Check if an element is a `<file/>` metadata element.
pub fn is_file_metadata_element(elem: &Element) -> bool {
    elem.ns() == NS_FILE_METADATA && elem.name() == "file"
}

/// Check if a message has file metadata.
pub fn has_file_metadata(msg: &Message) -> bool {
    msg.payloads.iter().any(|e| is_file_metadata_element(e))
}

// ── Extraction ───────────────────────────────────────────────────────

/// Extract file metadata from a message.
pub fn extract_file_metadata_from_message(msg: &Message) -> Option<FileMetadata> {
    msg.payloads
        .iter()
        .find(|e| is_file_metadata_element(e))
        .map(|e| parse_file_metadata_element(e))
}

/// Parse a `<file/>` element into FileMetadata.
pub fn parse_file_metadata_element(elem: &Element) -> FileMetadata {
    let text_child = |name: &str| -> Option<String> {
        elem.children()
            .find(|c| c.name() == name && c.ns() == NS_FILE_METADATA)
            .map(|c| c.text())
            .filter(|t| !t.is_empty())
    };

    let num_child = |name: &str| -> Option<u64> { text_child(name).and_then(|t| t.parse().ok()) };

    FileMetadata {
        media_type: text_child("media-type"),
        name: text_child("name"),
        size: num_child("size"),
        width: num_child("width").map(|v| v as u32),
        height: num_child("height").map(|v| v as u32),
        desc: text_child("desc"),
        duration: num_child("duration"),
    }
}

// ── Building ─────────────────────────────────────────────────────────

/// Build a `<file xmlns='urn:xmpp:file:metadata:0'>` element.
pub fn build_file_metadata_element(meta: &FileMetadata) -> Element {
    let mut file = Element::builder("file", NS_FILE_METADATA).build();

    let append_text = |parent: &mut Element, name: &str, value: &str| {
        let mut child = Element::builder(name, NS_FILE_METADATA).build();
        child.append_text_node(value);
        parent.append_child(child);
    };

    if let Some(ref mt) = meta.media_type {
        append_text(&mut file, "media-type", mt);
    }
    if let Some(ref name) = meta.name {
        append_text(&mut file, "name", name);
    }
    if let Some(size) = meta.size {
        append_text(&mut file, "size", &size.to_string());
    }
    if let Some(width) = meta.width {
        append_text(&mut file, "width", &width.to_string());
    }
    if let Some(height) = meta.height {
        append_text(&mut file, "height", &height.to_string());
    }
    if let Some(ref desc) = meta.desc {
        append_text(&mut file, "desc", desc);
    }
    if let Some(duration) = meta.duration {
        append_text(&mut file, "duration", &duration.to_string());
    }

    file
}

// ── Mutation ─────────────────────────────────────────────────────────

/// Add file metadata to a message, replacing any existing.
pub fn set_file_metadata(msg: &mut Message, meta: &FileMetadata) {
    msg.payloads.retain(|e| e.ns() != NS_FILE_METADATA);
    msg.payloads.push(build_file_metadata_element(meta));
}

/// Remove file metadata from a message.
pub fn strip_file_metadata(msg: &mut Message) {
    msg.payloads.retain(|e| e.ns() != NS_FILE_METADATA);
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::Message;

    #[test]
    fn test_is_file_metadata_element() {
        let elem = Element::builder("file", NS_FILE_METADATA).build();
        assert!(is_file_metadata_element(&elem));

        let wrong = Element::builder("file", "jabber:client").build();
        assert!(!is_file_metadata_element(&wrong));
    }

    #[test]
    fn test_parse_full_metadata() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <file xmlns='urn:xmpp:file:metadata:0'>\
                      <media-type>image/png</media-type>\
                      <name>photo.png</name>\
                      <size>12345</size>\
                      <width>800</width>\
                      <height>600</height>\
                      <desc>A sunset</desc>\
                    </file>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let meta = extract_file_metadata_from_message(&msg).expect("has metadata");
        assert_eq!(meta.media_type.as_deref(), Some("image/png"));
        assert_eq!(meta.name.as_deref(), Some("photo.png"));
        assert_eq!(meta.size, Some(12345));
        assert_eq!(meta.width, Some(800));
        assert_eq!(meta.height, Some(600));
        assert_eq!(meta.desc.as_deref(), Some("A sunset"));
        assert!(meta.is_image());
        assert!(!meta.is_video());
    }

    #[test]
    fn test_parse_minimal_metadata() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <file xmlns='urn:xmpp:file:metadata:0'>\
                      <name>doc.pdf</name>\
                    </file>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        let meta = extract_file_metadata_from_message(&msg).expect("has metadata");
        assert_eq!(meta.name.as_deref(), Some("doc.pdf"));
        assert_eq!(meta.media_type, None);
        assert_eq!(meta.size, None);
    }

    #[test]
    fn test_extract_absent() {
        let msg = Message::new(None::<jid::Jid>);
        assert!(extract_file_metadata_from_message(&msg).is_none());
    }

    #[test]
    fn test_build_metadata() {
        let meta = FileMetadata::new()
            .with_media_type("application/pdf")
            .with_name("report.pdf")
            .with_size(1048576)
            .with_desc("Annual report");

        let elem = build_file_metadata_element(&meta);
        assert_eq!(elem.name(), "file");
        assert_eq!(elem.ns(), NS_FILE_METADATA);

        let parsed = parse_file_metadata_element(&elem);
        assert_eq!(parsed.media_type.as_deref(), Some("application/pdf"));
        assert_eq!(parsed.name.as_deref(), Some("report.pdf"));
        assert_eq!(parsed.size, Some(1048576));
        assert_eq!(parsed.desc.as_deref(), Some("Annual report"));
    }

    #[test]
    fn test_build_with_dimensions() {
        let meta = FileMetadata::new()
            .with_media_type("video/mp4")
            .with_name("clip.mp4")
            .with_dimensions(1920, 1080)
            .with_duration(120);

        let elem = build_file_metadata_element(&meta);
        let parsed = parse_file_metadata_element(&elem);
        assert_eq!(parsed.width, Some(1920));
        assert_eq!(parsed.height, Some(1080));
        assert_eq!(parsed.duration, Some(120));
        assert!(parsed.is_video());
    }

    #[test]
    fn test_set_file_metadata() {
        let mut msg = Message::new(None::<jid::Jid>);
        let meta = FileMetadata::new().with_name("test.txt").with_size(100);
        set_file_metadata(&mut msg, &meta);

        assert!(has_file_metadata(&msg));
        let extracted = extract_file_metadata_from_message(&msg).expect("has metadata");
        assert_eq!(extracted.name.as_deref(), Some("test.txt"));

        // Replace
        let meta2 = FileMetadata::new().with_name("updated.txt");
        set_file_metadata(&mut msg, &meta2);
        let extracted2 = extract_file_metadata_from_message(&msg).expect("has metadata");
        assert_eq!(extracted2.name.as_deref(), Some("updated.txt"));
        assert_eq!(
            msg.payloads
                .iter()
                .filter(|e| e.ns() == NS_FILE_METADATA)
                .count(),
            1
        );
    }

    #[test]
    fn test_strip_file_metadata() {
        let mut msg = Message::new(None::<jid::Jid>);
        set_file_metadata(&mut msg, &FileMetadata::new().with_name("test.txt"));
        strip_file_metadata(&mut msg);
        assert!(!has_file_metadata(&msg));
    }

    #[test]
    fn test_file_metadata_carrier_trait() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <file xmlns='urn:xmpp:file:metadata:0'>\
                      <name>photo.jpg</name>\
                      <media-type>image/jpeg</media-type>\
                    </file>\
                    </message>";
        let msg =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("valid message");

        assert!(msg.has_file_metadata());
        let meta = msg.file_metadata().expect("has metadata");
        assert_eq!(meta.name.as_deref(), Some("photo.jpg"));
        assert!(meta.is_image());
    }

    #[test]
    fn test_is_media_type_helpers() {
        let img = FileMetadata::new().with_media_type("image/png");
        assert!(img.is_image());
        assert!(!img.is_video());
        assert!(!img.is_audio());

        let vid = FileMetadata::new().with_media_type("video/mp4");
        assert!(vid.is_video());

        let aud = FileMetadata::new().with_media_type("audio/ogg");
        assert!(aud.is_audio());

        let none = FileMetadata::new();
        assert!(!none.is_image());
    }

    #[test]
    fn test_human_size() {
        assert_eq!(
            FileMetadata::new().with_size(500).human_size().as_deref(),
            Some("500 B")
        );
        assert_eq!(
            FileMetadata::new().with_size(1536).human_size().as_deref(),
            Some("1.5 KB")
        );
        assert_eq!(
            FileMetadata::new()
                .with_size(5 * 1024 * 1024)
                .human_size()
                .as_deref(),
            Some("5.0 MB")
        );
        assert_eq!(FileMetadata::new().human_size(), None);
    }

    #[test]
    fn test_default() {
        let meta = FileMetadata::default();
        assert_eq!(meta.name, None);
        assert_eq!(meta.size, None);
    }

    #[test]
    fn test_builder_chain() {
        let meta = FileMetadata::new()
            .with_media_type("image/webp")
            .with_name("avatar.webp")
            .with_size(8192)
            .with_dimensions(256, 256)
            .with_desc("User avatar");

        assert_eq!(meta.media_type.as_deref(), Some("image/webp"));
        assert_eq!(meta.name.as_deref(), Some("avatar.webp"));
        assert_eq!(meta.size, Some(8192));
        assert_eq!(meta.width, Some(256));
        assert_eq!(meta.height, Some(256));
        assert_eq!(meta.desc.as_deref(), Some("User avatar"));
    }
}
