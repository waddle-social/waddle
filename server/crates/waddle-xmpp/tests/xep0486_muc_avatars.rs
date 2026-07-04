//! XEP-0486: MUC Avatars — dedicated suite.
//!
//! XEP-0486 reuses the XEP-0153 `vcard-temp:x:update` presence
//! advertisement for room avatars: the room's presence carries
//! `<x xmlns='vcard-temp:x:update'><photo>HASH</photo></x>` and
//! clients fetch the vCard photo when the hash is unknown.
//!
//! Pins:
//! - the `vcard-temp:x:update` namespace string,
//! - hash extraction from presence payload lists (positive, empty
//!   `<photo/>`, missing element, wrong namespace, multi-payload),
//! - `MucAvatar` / `MucAvatarCache` bookkeeping used by the client
//!   side to decide when a vCard fetch is needed.

use minidom::Element;
use waddle_xmpp::xep::xep0486::{
    extract_avatar_hash_from_presence, MucAvatar, MucAvatarCache, NS_VCARD_UPDATE,
};

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn xep0486_vcard_update_namespace_matches_xep0153() {
    // xep-0486.xml builds on XEP-0153's presence advertisement.
    assert_eq!(NS_VCARD_UPDATE, "vcard-temp:x:update");
}

// ── Presence hash extraction ─────────────────────────────────────────

#[test]
fn xep0486_extracts_hash_from_spec_shaped_presence_payload() {
    let x: Element = "<x xmlns='vcard-temp:x:update'>\
            <photo>186f1f50f0eef4c5b0ee16b13a02d90495ea0367</photo>\
        </x>"
        .parse()
        .expect("valid xml");
    assert_eq!(
        extract_avatar_hash_from_presence(&[x]),
        Some("186f1f50f0eef4c5b0ee16b13a02d90495ea0367".to_owned())
    );
}

#[test]
fn xep0486_empty_photo_element_yields_none() {
    // `<photo/>` means "no avatar" per XEP-0153 §4.2 — it must not
    // surface as an empty-string hash.
    let x: Element = "<x xmlns='vcard-temp:x:update'><photo/></x>"
        .parse()
        .expect("valid xml");
    assert_eq!(extract_avatar_hash_from_presence(&[x]), None);
}

#[test]
fn xep0486_missing_photo_child_yields_none() {
    let x: Element = "<x xmlns='vcard-temp:x:update'/>"
        .parse()
        .expect("valid xml");
    assert_eq!(extract_avatar_hash_from_presence(&[x]), None);
}

#[test]
fn xep0486_wrong_namespace_x_element_is_ignored() {
    let x: Element = "<x xmlns='jabber:x:conference'><photo>abc</photo></x>"
        .parse()
        .expect("valid xml");
    assert_eq!(extract_avatar_hash_from_presence(&[x]), None);
    assert_eq!(extract_avatar_hash_from_presence(&[]), None);
}

#[test]
fn xep0486_finds_vcard_update_among_other_presence_payloads() {
    // MUC presence typically carries `<x xmlns='...muc#user'>` and a
    // XEP-0115 `<c/>` alongside the avatar advertisement.
    let muc_user: Element = "<x xmlns='http://jabber.org/protocol/muc#user'/>"
        .parse()
        .expect("valid xml");
    let caps: Element =
        "<c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='n' ver='v'/>"
            .parse()
            .expect("valid xml");
    let update: Element = "<x xmlns='vcard-temp:x:update'><photo>roomhash1</photo></x>"
        .parse()
        .expect("valid xml");

    assert_eq!(
        extract_avatar_hash_from_presence(&[muc_user, caps, update]),
        Some("roomhash1".to_owned())
    );
}

// ── Avatar model ─────────────────────────────────────────────────────

#[test]
fn xep0486_avatar_presence_requires_hash_or_url() {
    assert!(!MucAvatar::new("room@muc").has_avatar());
    assert!(MucAvatar::new("room@muc").with_hash("h").has_avatar());
    assert!(MucAvatar::new("room@muc")
        .with_url("https://example.com/a.png")
        .has_avatar());
}

// ── Cache bookkeeping ────────────────────────────────────────────────

#[test]
fn xep0486_cache_set_get_and_hash_lookup() {
    let mut cache = MucAvatarCache::new();
    cache.set(MucAvatar::new("room@muc").with_hash("abc123"));

    assert!(cache.has_avatar("room@muc"));
    assert_eq!(cache.photo_hash("room@muc"), Some("abc123"));
    assert_eq!(
        cache.get("room@muc").map(|a| a.room_jid.as_str()),
        Some("room@muc")
    );
}

#[test]
fn xep0486_cache_update_replaces_previous_entry() {
    // A room presence with a new hash (avatar changed) must replace
    // the stale entry so clients re-fetch the vCard exactly once.
    let mut cache = MucAvatarCache::new();
    cache.set(MucAvatar::new("room@muc").with_hash("old"));
    cache.set(MucAvatar::new("room@muc").with_hash("new"));
    assert_eq!(cache.photo_hash("room@muc"), Some("new"));
}

#[test]
fn xep0486_cache_entry_without_avatar_reports_no_avatar() {
    // A cached room known to have NO avatar (empty `<photo/>`) is
    // different from an unknown room: both report false but the
    // entry existence prevents redundant vCard fetches.
    let mut cache = MucAvatarCache::new();
    cache.set(MucAvatar::new("bare@muc"));

    assert!(!cache.has_avatar("bare@muc"));
    assert!(cache.get("bare@muc").is_some());
    assert_eq!(cache.photo_hash("bare@muc"), None);
    assert!(!cache.has_avatar("never-seen@muc"));
    assert!(cache.get("never-seen@muc").is_none());
}

#[test]
fn xep0486_cache_remove_forgets_room() {
    let mut cache = MucAvatarCache::new();
    cache.set(MucAvatar::new("room@muc").with_hash("h"));
    cache.remove("room@muc");
    assert!(!cache.has_avatar("room@muc"));
    assert!(cache.get("room@muc").is_none());
}

#[test]
fn xep0486_rooms_with_avatars_filters_avatarless_entries() {
    let mut cache = MucAvatarCache::new();
    cache.set(MucAvatar::new("a@muc").with_hash("h1"));
    cache.set(MucAvatar::new("b@muc"));
    cache.set(MucAvatar::new("c@muc").with_url("https://example.com/c.png"));

    let mut rooms = cache.rooms_with_avatars();
    rooms.sort_unstable();
    assert_eq!(rooms, vec!["a@muc", "c@muc"]);
}

// ── End-to-end: presence → cache ─────────────────────────────────────

#[test]
fn xep0486_presence_hash_feeds_cache_round_trip() {
    let update: Element = "<x xmlns='vcard-temp:x:update'><photo>feedbeef</photo></x>"
        .parse()
        .expect("valid xml");
    let hash = extract_avatar_hash_from_presence(&[update]).expect("hash present");

    let mut cache = MucAvatarCache::new();
    cache.set(MucAvatar::new("room@muc.example.com").with_hash(hash));

    assert_eq!(cache.photo_hash("room@muc.example.com"), Some("feedbeef"));
}
