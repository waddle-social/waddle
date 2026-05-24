//! Push notification envelope: typed plaintext for the chat service
//! worker and per-class transport policy (TTL / Urgency / Topic).
//!
//! The envelope is the only thing the service worker sees after AES-GCM
//! decryption. It is JSON-serialized with a `"v": 1` schema discriminator
//! so the SW can switch on it cleanly when PR-D3 lands.
//!
//! Per-class policy is held inside [`PushClass`] (bucket size, TTL,
//! urgency) so the publish-job worker doesn't have to plumb three
//! correlated parameters through every call site — picking the class is
//! enough.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::constants::{DEFAULT_PLAINTEXT_BUCKET, DM_PLAINTEXT_BUCKET};
use super::sender::Urgency;
use super::types::{PushTopic, PushTopicParseError};

/// Push notification classes recognized by the chat service worker.
///
/// Per-class TTL / Urgency / bucket-size are encapsulated here so the
/// publish-job worker selects policy by class rather than wiring three
/// correlated parameters per call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushClass {
    /// One-to-one direct message. Larger plaintext bucket (1024 bytes)
    /// covers most DM bodies without splitting; `Urgency::High`; longer
    /// TTL — DMs are user-personal and should survive offline periods.
    DirectMessage,
    /// Mention in a multi-user room. Small bucket (256 bytes); high
    /// urgency; medium TTL — mentions are time-sensitive and shouldn't
    /// fan out a 1 KiB body for every device.
    Mention,
    /// General room notification (non-mention). Small bucket; normal
    /// urgency; short TTL — coalesce aggressively.
    Room,
}

impl PushClass {
    /// Plaintext bucket size fed to [`super::encrypt::encrypt`] —
    /// padding target so encrypted body length never leaks plaintext
    /// length.
    pub fn bucket_size(self) -> usize {
        match self {
            PushClass::DirectMessage => DM_PLAINTEXT_BUCKET,
            PushClass::Mention | PushClass::Room => DEFAULT_PLAINTEXT_BUCKET,
        }
    }

    /// RFC 8030 §5.2 `TTL` header in seconds — how long the relay
    /// should hold the message if the device is offline.
    pub fn ttl(self) -> u32 {
        match self {
            PushClass::DirectMessage => 7 * 24 * 60 * 60, // 7 days
            PushClass::Mention => 24 * 60 * 60,           // 1 day
            PushClass::Room => 4 * 60 * 60,               // 4 hours
        }
    }

    /// RFC 8030 §5.3 `Urgency` header. DMs/mentions wake the device;
    /// general room traffic stays at normal so battery-saver delays it.
    pub fn urgency(self) -> Urgency {
        match self {
            PushClass::DirectMessage | PushClass::Mention => Urgency::High,
            PushClass::Room => Urgency::Normal,
        }
    }

    fn discriminant_byte(self) -> u8 {
        match self {
            PushClass::DirectMessage => b'd',
            PushClass::Mention => b'm',
            PushClass::Room => b'r',
        }
    }
}

/// Map a granular notification class (the db-form string emitted by the
/// chat client into `<context xmlns='urn:waddle:push:context:0' class='...'/>`)
/// to the transport-policy [`PushClass`].
///
/// Unknown values fall through to [`PushClass::Mention`] (small bucket,
/// high urgency) — the safer side: small bucket can never leak more
/// than ~256 bytes of plaintext-length, and high urgency wakes the
/// device. Logging the unknown value is the caller's responsibility.
pub fn push_class_for_db_value(class: &str) -> PushClass {
    match class {
        "dm" | "dm_mention" => PushClass::DirectMessage,
        "personal_mention" | "channel_mention" | "active_channel_mention" | "notify_all" => {
            PushClass::Mention
        }
        _ => PushClass::Mention,
    }
}

/// `"v": 1` envelope serialized into the encrypted body.
///
/// Field names are intentionally short — every byte after padding
/// counts towards the relay's body-size ceiling.
///
/// XEP-0357 §4 forbids the push service from receiving message
/// content, so this envelope carries only routing metadata; the chat
/// service worker uses it to decide *how* to render a generic
/// "new message" notification and to wake the app for the real body.
#[derive(Debug, Clone, Serialize)]
pub struct PushEnvelope<'a> {
    /// Schema version. Always `1` for this PR; PR-D3's SW switches on
    /// it.
    pub v: u8,
    /// Granular notification class (e.g. `"dm"`, `"personal_mention"`,
    /// `"channel_mention"`). Carries finer detail than the transport
    /// [`PushClass`] so the SW can pick the right localized banner.
    pub class: &'a str,
    /// Conversation bare JID (DM peer or MUC room).
    pub conversation: &'a str,
    /// XEP-0201 thread id, if the publishing chat client included one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<&'a str>,
    /// Stanza item id of the originating message — lets the SW
    /// deduplicate against an earlier in-band notification.
    pub item: &'a str,
    /// Server-side unread count snapshot (XEP-0357 `message-count`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread: Option<u64>,
}

impl<'a> PushEnvelope<'a> {
    pub fn new(
        class: &'a str,
        conversation: &'a str,
        thread: Option<&'a str>,
        item: &'a str,
        unread: Option<u64>,
    ) -> Self {
        Self {
            v: 1,
            class,
            conversation,
            thread,
            item,
            unread,
        }
    }

    /// Serialize to the byte string that gets fed to
    /// [`super::encrypt::encrypt`]. JSON is canonical-ish (serde's
    /// struct order is the declaration order) but the SW does not
    /// require canonicalization.
    pub fn to_plaintext(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("envelope serializes as JSON")
    }
}

/// Build an RFC 8030 §5.4 `Topic` for `(class, conversation)`.
///
/// The relay uses `Topic` to coalesce queued messages: a newer push
/// with the same topic replaces an older one. We hash both inputs to
/// avoid leaking the bare JID to the relay (and to fit the §5.4 32-char
/// ceiling regardless of JID length).
///
/// Output: 32 base64url-no-pad characters over the first 24 bytes of
/// `SHA-256(class-discriminant || 0x00 || conversation)`. 192 bits is
/// plenty of collision resistance for per-(class, conversation)
/// dedup-keying.
pub fn push_topic_for(class: PushClass, conversation: &str) -> PushTopic {
    let mut hasher = Sha256::new();
    hasher.update([class.discriminant_byte()]);
    hasher.update([0u8]); // unambiguous separator so `(d, "abc")` ≠ `(da, "bc")`
    hasher.update(conversation.as_bytes());
    let digest = hasher.finalize();
    let encoded = URL_SAFE_NO_PAD.encode(&digest[..24]);
    debug_assert_eq!(encoded.len(), 32);
    PushTopic::new(encoded).unwrap_or_else(|err: PushTopicParseError| {
        // Cannot happen: base64url alphabet is a strict subset of the
        // RFC 8030 §5.4 topic-char alphabet, and 24 bytes → exactly 32
        // chars stays at the 32-char ceiling. Surface as an assert so a
        // future input change is caught loudly rather than silently
        // truncated.
        panic!("push_topic_for produced invalid PushTopic: {err:?}");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_bucket_size_picks_dm_vs_default() {
        assert_eq!(PushClass::DirectMessage.bucket_size(), DM_PLAINTEXT_BUCKET);
        assert_eq!(PushClass::Mention.bucket_size(), DEFAULT_PLAINTEXT_BUCKET);
        assert_eq!(PushClass::Room.bucket_size(), DEFAULT_PLAINTEXT_BUCKET);
    }

    #[test]
    fn class_ttl_is_per_class() {
        assert!(PushClass::DirectMessage.ttl() > PushClass::Mention.ttl());
        assert!(PushClass::Mention.ttl() > PushClass::Room.ttl());
    }

    #[test]
    fn class_urgency_is_per_class() {
        assert_eq!(PushClass::DirectMessage.urgency(), Urgency::High);
        assert_eq!(PushClass::Mention.urgency(), Urgency::High);
        assert_eq!(PushClass::Room.urgency(), Urgency::Normal);
    }

    #[test]
    fn envelope_serializes_v1_schema() {
        let env = PushEnvelope::new(
            "dm",
            "alice@example.com",
            Some("thread-abc"),
            "item-123",
            Some(3),
        );
        let bytes = env.to_plaintext();
        let s = std::str::from_utf8(&bytes).unwrap();
        let json: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(json["v"], 1);
        assert_eq!(json["class"], "dm");
        assert_eq!(json["conversation"], "alice@example.com");
        assert_eq!(json["thread"], "thread-abc");
        assert_eq!(json["item"], "item-123");
        assert_eq!(json["unread"], 3);
    }

    #[test]
    fn envelope_omits_optional_fields_when_absent() {
        let env = PushEnvelope::new("dm", "alice@example.com", None, "item-x", None);
        let json: serde_json::Value = serde_json::from_slice(&env.to_plaintext()).unwrap();
        assert!(json.get("unread").is_none(), "unread must be omitted");
        assert!(json.get("thread").is_none(), "thread must be omitted");
    }

    #[test]
    fn push_class_for_db_value_maps_granular_classes() {
        assert_eq!(push_class_for_db_value("dm"), PushClass::DirectMessage);
        assert_eq!(
            push_class_for_db_value("dm_mention"),
            PushClass::DirectMessage
        );
        assert_eq!(
            push_class_for_db_value("personal_mention"),
            PushClass::Mention
        );
        assert_eq!(
            push_class_for_db_value("channel_mention"),
            PushClass::Mention
        );
        assert_eq!(
            push_class_for_db_value("active_channel_mention"),
            PushClass::Mention
        );
        assert_eq!(push_class_for_db_value("notify_all"), PushClass::Mention);
        // Unknown falls through to Mention (safer-side default).
        assert_eq!(push_class_for_db_value("anything-else"), PushClass::Mention);
    }

    #[test]
    fn topic_is_32_chars_and_alphabet_valid() {
        let topic = push_topic_for(PushClass::Mention, "room@conf.example.com");
        assert_eq!(topic.as_str().len(), 32);
        // Constructor enforces RFC 8030 §5.4 alphabet — the round-trip
        // proves it.
        let again = PushTopic::new(topic.as_str().to_string()).expect("alphabet-valid");
        assert_eq!(again.as_str(), topic.as_str());
    }

    #[test]
    fn topic_differs_by_class() {
        let dm = push_topic_for(PushClass::DirectMessage, "alice@example.com");
        let mention = push_topic_for(PushClass::Mention, "alice@example.com");
        let room = push_topic_for(PushClass::Room, "alice@example.com");
        assert_ne!(dm.as_str(), mention.as_str());
        assert_ne!(mention.as_str(), room.as_str());
        assert_ne!(dm.as_str(), room.as_str());
    }

    #[test]
    fn topic_is_stable_for_same_inputs() {
        let a = push_topic_for(PushClass::DirectMessage, "alice@example.com");
        let b = push_topic_for(PushClass::DirectMessage, "alice@example.com");
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn topic_separator_prevents_class_concat_collision() {
        // Without the 0x00 separator, `(class='d', conv='abc')` and
        // `(class='da', conv='bc')` would hash to the same bytes. The
        // explicit separator + single-byte discriminant prevents that.
        // We can't synthesize a PushClass::DaBC, but we can prove the
        // hash includes the separator by replicating the construction.
        let mut h = sha2::Sha256::new();
        h.update([b'd', 0u8]);
        h.update(b"abc");
        let expected = URL_SAFE_NO_PAD.encode(&h.finalize()[..24]);
        let topic = push_topic_for(PushClass::DirectMessage, "abc");
        assert_eq!(topic.as_str(), expected);
    }
}
