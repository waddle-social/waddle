//! XEP-0486: MUC Avatars
//!
//! Room/channel avatars for MUC. Each room can have a custom icon
//! published via its vCard, with a hash advertised in presence.
//!
//! ## Protocol Flow
//!
//! 1. Room publishes vCard with PHOTO via owner
//! 2. Room presence includes avatar hash in `<x xmlns='vcard-temp:x:update'>`
//! 3. Clients cache avatar by hash, fetch vCard if unknown hash
//!
//! ## Use Cases
//!
//! - Custom channel icons in sidebar
//! - Room branding in header

/// Namespace for vCard avatar updates in presence.
pub const NS_VCARD_UPDATE: &str = "vcard-temp:x:update";

/// A MUC room avatar reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MucAvatar {
    /// The room JID.
    pub room_jid: String,
    /// SHA-1 hash of the avatar image data.
    pub photo_hash: Option<String>,
    /// Direct URL to the avatar image (if available via HTTP).
    pub url: Option<String>,
}

impl MucAvatar {
    /// Create a new MUC avatar reference.
    pub fn new(room_jid: impl Into<String>) -> Self {
        Self {
            room_jid: room_jid.into(),
            photo_hash: None,
            url: None,
        }
    }

    /// Set the photo hash.
    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.photo_hash = Some(hash.into());
        self
    }

    /// Set a direct URL.
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Returns `true` if the room has an avatar.
    pub fn has_avatar(&self) -> bool {
        self.photo_hash.is_some() || self.url.is_some()
    }
}

/// Cache of room avatar hashes for efficient lookup.
#[derive(Debug, Default)]
pub struct MucAvatarCache {
    avatars: std::collections::HashMap<String, MucAvatar>,
}

impl MucAvatarCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update avatar for a room.
    pub fn set(&mut self, avatar: MucAvatar) {
        self.avatars.insert(avatar.room_jid.clone(), avatar);
    }

    /// Get avatar for a room.
    pub fn get(&self, room_jid: &str) -> Option<&MucAvatar> {
        self.avatars.get(room_jid)
    }

    /// Check if a room has a known avatar.
    pub fn has_avatar(&self, room_jid: &str) -> bool {
        self.avatars.get(room_jid).is_some_and(|a| a.has_avatar())
    }

    /// Get the photo hash for a room.
    pub fn photo_hash(&self, room_jid: &str) -> Option<&str> {
        self.avatars
            .get(room_jid)
            .and_then(|a| a.photo_hash.as_deref())
    }

    /// Remove avatar for a room.
    pub fn remove(&mut self, room_jid: &str) {
        self.avatars.remove(room_jid);
    }

    /// All rooms with avatars.
    pub fn rooms_with_avatars(&self) -> Vec<&str> {
        self.avatars
            .iter()
            .filter(|(_, a)| a.has_avatar())
            .map(|(jid, _)| jid.as_str())
            .collect()
    }
}

/// Extract avatar hash from a MUC presence vCard update.
pub fn extract_avatar_hash_from_presence(payloads: &[minidom::Element]) -> Option<String> {
    payloads
        .iter()
        .find(|e| e.name() == "x" && e.ns() == NS_VCARD_UPDATE)
        .and_then(|x| x.children().find(|c| c.name() == "photo"))
        .map(|photo| photo.text())
        .filter(|hash| !hash.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_muc_avatar_new() {
        let a = MucAvatar::new("room@muc")
            .with_hash("abc123")
            .with_url("https://example.com/avatar.png");
        assert!(a.has_avatar());
        assert_eq!(a.photo_hash.as_deref(), Some("abc123"));
        assert_eq!(a.url.as_deref(), Some("https://example.com/avatar.png"));
    }

    #[test]
    fn test_muc_avatar_no_avatar() {
        let a = MucAvatar::new("room@muc");
        assert!(!a.has_avatar());
    }

    #[test]
    fn test_cache_set_get() {
        let mut cache = MucAvatarCache::new();
        cache.set(MucAvatar::new("room@muc").with_hash("abc"));
        assert!(cache.has_avatar("room@muc"));
        assert_eq!(cache.photo_hash("room@muc"), Some("abc"));
    }

    #[test]
    fn test_cache_miss() {
        let cache = MucAvatarCache::new();
        assert!(!cache.has_avatar("unknown@muc"));
        assert_eq!(cache.photo_hash("unknown@muc"), None);
    }

    #[test]
    fn test_cache_remove() {
        let mut cache = MucAvatarCache::new();
        cache.set(MucAvatar::new("room@muc").with_hash("abc"));
        cache.remove("room@muc");
        assert!(!cache.has_avatar("room@muc"));
    }

    #[test]
    fn test_cache_rooms_with_avatars() {
        let mut cache = MucAvatarCache::new();
        cache.set(MucAvatar::new("a@muc").with_hash("h1"));
        cache.set(MucAvatar::new("b@muc")); // no avatar
        cache.set(MucAvatar::new("c@muc").with_url("url"));

        let rooms = cache.rooms_with_avatars();
        assert_eq!(rooms.len(), 2);
    }

    #[test]
    fn test_extract_avatar_hash() {
        let xml = "<x xmlns='vcard-temp:x:update'><photo>abc123def</photo></x>";
        let elem: minidom::Element = xml.parse().expect("valid");
        let hash = extract_avatar_hash_from_presence(&[elem]);
        assert_eq!(hash, Some("abc123def".to_owned()));
    }

    #[test]
    fn test_extract_avatar_hash_empty() {
        let xml = "<x xmlns='vcard-temp:x:update'><photo/></x>";
        let elem: minidom::Element = xml.parse().expect("valid");
        assert!(extract_avatar_hash_from_presence(&[elem]).is_none());
    }

    #[test]
    fn test_extract_avatar_hash_missing() {
        let xml = "<x xmlns='jabber:client'/>";
        let elem: minidom::Element = xml.parse().expect("valid");
        assert!(extract_avatar_hash_from_presence(&[elem]).is_none());
    }
}
