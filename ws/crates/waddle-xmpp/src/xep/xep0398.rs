//! XEP-0398: User Avatar to vCard-Based Avatars Conversion
//!
//! Bridges XEP-0084 (PEP avatars) and XEP-0153 (vCard avatars) bidirectionally.
//! When a user updates their avatar via either mechanism, the server ensures
//! the other representation is updated to match.

use super::xep0084::AvatarInfo;
use super::xep0153;

/// Namespace for the PEP-vCard conversion feature.
pub const NS_PEP_VCARD_CONVERSION: &str = "urn:xmpp:pep-vcard-conversion:0";

/// Trait for avatar conversion between PEP and vCard formats.
///
/// Per CLAUDE.md: use traits for extensibility.
pub trait AvatarConversion: Send + Sync {
    /// Called when the user's vCard PHOTO is updated.
    ///
    /// Should publish the photo data to PEP avatar nodes
    /// (urn:xmpp:avatar:data and urn:xmpp:avatar:metadata).
    fn on_vcard_photo_updated(
        &self,
        photo_base64: &str,
        mime_type: &str,
    ) -> Option<(AvatarInfo, String)>;

    /// Called when the user publishes to the PEP avatar nodes.
    ///
    /// Should update the vCard PHOTO field to match.
    fn on_pep_avatar_published(
        &self,
        avatar_data_base64: &str,
        metadata: &AvatarInfo,
    ) -> Option<(String, String)>;
}

/// Default implementation of avatar conversion.
pub struct DefaultAvatarConversion;

impl AvatarConversion for DefaultAvatarConversion {
    fn on_vcard_photo_updated(
        &self,
        photo_base64: &str,
        mime_type: &str,
    ) -> Option<(AvatarInfo, String)> {
        let (info, _) = vcard_photo_to_pep_avatar(photo_base64, mime_type)?;
        Some((info, photo_base64.to_string()))
    }

    fn on_pep_avatar_published(
        &self,
        avatar_data_base64: &str,
        metadata: &AvatarInfo,
    ) -> Option<(String, String)> {
        Some(pep_avatar_to_vcard_photo(avatar_data_base64, metadata))
    }
}

/// Convert vCard PHOTO data to PEP avatar metadata + data.
///
/// Returns (AvatarInfo, base64_data) for publishing to PEP nodes.
pub fn vcard_photo_to_pep_avatar(
    photo_base64: &str,
    mime_type: &str,
) -> Option<(AvatarInfo, String)> {
    let hash = xep0153::compute_photo_hash_from_base64(photo_base64)?;

    use base64::{engine::general_purpose::STANDARD, Engine};
    let data = STANDARD.decode(photo_base64).ok()?;

    let info = AvatarInfo {
        id: hash,
        mime_type: mime_type.to_string(),
        width: None,
        height: None,
        bytes: Some(data.len() as u64),
        url: None,
    };

    Some((info, photo_base64.to_string()))
}

/// Convert PEP avatar to vCard PHOTO data.
///
/// Returns (base64_data, mime_type) for storing in vCard.
pub fn pep_avatar_to_vcard_photo(
    avatar_data_base64: &str,
    metadata: &AvatarInfo,
) -> (String, String) {
    (avatar_data_base64.to_string(), metadata.mime_type.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vcard_photo_to_pep_avatar() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let data = b"fake image data";
        let base64 = STANDARD.encode(data);

        let result = vcard_photo_to_pep_avatar(&base64, "image/png");
        assert!(result.is_some());

        let (info, data_back) = result.unwrap();
        assert!(!info.id.is_empty());
        assert_eq!(info.mime_type, "image/png");
        assert_eq!(info.bytes, Some(data.len() as u64));
        assert_eq!(data_back, base64);
    }

    #[test]
    fn test_pep_avatar_to_vcard_photo() {
        let info = AvatarInfo {
            id: "abc123".to_string(),
            mime_type: "image/jpeg".to_string(),
            width: None,
            height: None,
            bytes: None,
            url: None,
        };

        let (data, mime) = pep_avatar_to_vcard_photo("base64data", &info);
        assert_eq!(data, "base64data");
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn test_default_conversion_trait() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let converter = DefaultAvatarConversion;
        let data = b"fake image";
        let base64 = STANDARD.encode(data);

        let result = converter.on_vcard_photo_updated(&base64, "image/png");
        assert!(result.is_some());
    }
}
