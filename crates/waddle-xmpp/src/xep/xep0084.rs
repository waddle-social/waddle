//! XEP-0084: User Avatar
//!
//! PEP-based avatar storage using two nodes:
//! - `urn:xmpp:avatar:data` — raw avatar image data
//! - `urn:xmpp:avatar:metadata` — avatar metadata (MIME type, dimensions, hash)
//!
//! The PEP infrastructure already recognizes these as well-known nodes.

use minidom::Element;
use sha1::{Digest, Sha1};

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Namespace for avatar data.
pub const NS_AVATAR_DATA: &str = "urn:xmpp:avatar:data";

/// Namespace for avatar metadata.
pub const NS_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";

/// PEP node for avatar data.
pub const NODE_AVATAR_DATA: &str = "urn:xmpp:avatar:data";

/// PEP node for avatar metadata.
pub const NODE_AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";

/// Avatar metadata information.
#[derive(Debug, Clone)]
pub struct AvatarInfo {
    /// SHA-1 hash of the image data (hex-encoded, used as item id).
    pub id: String,
    /// MIME type of the image.
    pub mime_type: String,
    /// Image width in pixels.
    pub width: Option<u32>,
    /// Image height in pixels.
    pub height: Option<u32>,
    /// Image size in bytes.
    pub bytes: Option<u64>,
    /// Optional URL for the avatar image.
    pub url: Option<String>,
}

/// Check if a node name is the avatar data node.
pub fn is_avatar_data_node(node: &str) -> bool {
    node == NODE_AVATAR_DATA
}

/// Check if a node name is the avatar metadata node.
pub fn is_avatar_metadata_node(node: &str) -> bool {
    node == NODE_AVATAR_METADATA
}

/// Compute SHA-1 hash of avatar data (raw bytes), returning hex string.
pub fn compute_avatar_hash(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex_encode(&result)
}

/// Parse avatar metadata from a `<metadata>` element.
pub fn parse_avatar_metadata(element: &Element) -> Option<AvatarInfo> {
    if element.name() != "metadata" || element.ns() != NS_AVATAR_METADATA {
        return None;
    }

    let info_elem = element
        .children()
        .find(|c| c.name() == "info" && c.ns() == NS_AVATAR_METADATA)?;

    let id = info_elem.attr("id")?.to_string();
    let mime_type = info_elem.attr("type").unwrap_or("image/png").to_string();
    let width = info_elem.attr("width").and_then(|w| w.parse().ok());
    let height = info_elem.attr("height").and_then(|h| h.parse().ok());
    let bytes = info_elem.attr("bytes").and_then(|b| b.parse().ok());
    let url = info_elem.attr("url").map(|u| u.to_string());

    Some(AvatarInfo {
        id,
        mime_type,
        width,
        height,
        bytes,
        url,
    })
}

/// Build avatar metadata element.
pub fn build_avatar_metadata(info: &AvatarInfo) -> Element {
    let mut info_builder = Element::builder("info", NS_AVATAR_METADATA)
        .attr("id", &info.id)
        .attr("type", &info.mime_type);

    if let Some(width) = info.width {
        info_builder = info_builder.attr("width", width.to_string());
    }
    if let Some(height) = info.height {
        info_builder = info_builder.attr("height", height.to_string());
    }
    if let Some(bytes) = info.bytes {
        info_builder = info_builder.attr("bytes", bytes.to_string());
    }
    if let Some(ref url) = info.url {
        info_builder = info_builder.attr("url", url);
    }

    Element::builder("metadata", NS_AVATAR_METADATA)
        .append(info_builder.build())
        .build()
}

/// Parse avatar data from a `<data>` element.
///
/// Returns the base64-encoded image data.
pub fn parse_avatar_data(element: &Element) -> Option<String> {
    if element.name() == "data" && element.ns() == NS_AVATAR_DATA {
        let text = element.text();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    } else {
        None
    }
}

/// Build avatar data element with base64-encoded content.
pub fn build_avatar_data(base64_data: &str) -> Element {
    Element::builder("data", NS_AVATAR_DATA)
        .append(minidom::Node::Text(base64_data.to_string()))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_avatar_nodes() {
        assert!(is_avatar_data_node("urn:xmpp:avatar:data"));
        assert!(is_avatar_metadata_node("urn:xmpp:avatar:metadata"));
        assert!(!is_avatar_data_node("urn:xmpp:avatar:metadata"));
        assert!(!is_avatar_metadata_node("urn:xmpp:avatar:data"));
    }

    #[test]
    fn test_compute_avatar_hash() {
        let data = b"test image data";
        let hash = compute_avatar_hash(data);
        assert!(!hash.is_empty());
        // SHA-1 hash should be 40 hex chars
        assert_eq!(hash.len(), 40);
    }

    #[test]
    fn test_build_and_parse_metadata() {
        let info = AvatarInfo {
            id: "abc123".to_string(),
            mime_type: "image/png".to_string(),
            width: Some(64),
            height: Some(64),
            bytes: Some(1024),
            url: None,
        };

        let elem = build_avatar_metadata(&info);
        let parsed = parse_avatar_metadata(&elem);

        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.id, "abc123");
        assert_eq!(parsed.mime_type, "image/png");
        assert_eq!(parsed.width, Some(64));
        assert_eq!(parsed.height, Some(64));
        assert_eq!(parsed.bytes, Some(1024));
    }

    #[test]
    fn test_build_and_parse_data() {
        let base64 = "aW1hZ2UgZGF0YQ=="; // "image data" in base64
        let elem = build_avatar_data(base64);
        let parsed = parse_avatar_data(&elem);

        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap(), base64);
    }
}
