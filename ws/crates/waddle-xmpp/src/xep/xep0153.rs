//! XEP-0153: vCard-Based Avatars
//!
//! Broadcasts vCard PHOTO hash in presence via `<x xmlns='vcard-temp:x:update'>`.
//! This allows clients to know when another user's avatar has changed
//! by including a hash in every presence broadcast.

use minidom::Element;
use sha1::{Digest, Sha1};

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Namespace for vCard avatar update in presence.
pub const NS_VCARD_UPDATE: &str = "vcard-temp:x:update";

/// Build a vCard update element for inclusion in presence stanzas.
///
/// If `photo_hash` is Some, includes the hash. If None, includes an empty
/// `<photo/>` element indicating the user has no avatar.
pub fn build_vcard_update_element(photo_hash: Option<&str>) -> Element {
    let mut builder = Element::builder("x", NS_VCARD_UPDATE);

    let photo_elem = match photo_hash {
        Some(hash) => Element::builder("photo", NS_VCARD_UPDATE)
            .append(minidom::Node::Text(hash.to_string()))
            .build(),
        None => Element::builder("photo", NS_VCARD_UPDATE).build(),
    };

    builder = builder.append(photo_elem);
    builder.build()
}

/// Parse a vCard update element from a presence stanza.
///
/// Returns the photo hash if present, None if empty or missing.
pub fn parse_vcard_update(element: &Element) -> Option<String> {
    if element.name() != "x" || element.ns() != NS_VCARD_UPDATE {
        return None;
    }

    element
        .children()
        .find(|c| c.name() == "photo" && c.ns() == NS_VCARD_UPDATE)
        .and_then(|photo| {
            let text = photo.text();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        })
}

/// Check if an element is a vCard update element.
pub fn has_vcard_update(element: &Element) -> bool {
    element.name() == "x" && element.ns() == NS_VCARD_UPDATE
}

/// Compute SHA-1 hash of raw photo data (binary), returning hex string.
pub fn compute_photo_hash(data: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex_encode(&result)
}

/// Compute SHA-1 hash from base64-encoded photo data.
///
/// Returns None if the base64 data is invalid.
pub fn compute_photo_hash_from_base64(base64_data: &str) -> Option<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let data = STANDARD.decode(base64_data).ok()?;
    Some(compute_photo_hash(&data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_vcard_update_with_hash() {
        let elem = build_vcard_update_element(Some("abc123def456"));
        assert_eq!(elem.name(), "x");
        assert_eq!(elem.ns(), NS_VCARD_UPDATE);

        let photo = elem.children().find(|c| c.name() == "photo");
        assert!(photo.is_some());
        assert_eq!(photo.unwrap().text(), "abc123def456");
    }

    #[test]
    fn test_build_vcard_update_empty() {
        let elem = build_vcard_update_element(None);
        assert_eq!(elem.name(), "x");
        assert_eq!(elem.ns(), NS_VCARD_UPDATE);

        let photo = elem.children().find(|c| c.name() == "photo");
        assert!(photo.is_some());
        assert!(photo.unwrap().text().is_empty());
    }

    #[test]
    fn test_parse_vcard_update() {
        let elem = build_vcard_update_element(Some("abc123"));
        let hash = parse_vcard_update(&elem);
        assert_eq!(hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_parse_vcard_update_empty() {
        let elem = build_vcard_update_element(None);
        let hash = parse_vcard_update(&elem);
        assert!(hash.is_none());
    }

    #[test]
    fn test_has_vcard_update() {
        let elem = build_vcard_update_element(Some("abc"));
        assert!(has_vcard_update(&elem));

        let other = Element::builder("x", "some:other:ns").build();
        assert!(!has_vcard_update(&other));
    }

    #[test]
    fn test_compute_photo_hash() {
        let data = b"test photo data";
        let hash = compute_photo_hash(data);
        assert_eq!(hash.len(), 40); // SHA-1 produces 40 hex chars
    }

    #[test]
    fn test_compute_photo_hash_from_base64() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let data = b"test photo data";
        let encoded = STANDARD.encode(data);
        let hash = compute_photo_hash_from_base64(&encoded);
        assert!(hash.is_some());
        assert_eq!(hash.unwrap(), compute_photo_hash(data));
    }
}
