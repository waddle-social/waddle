//! MUC pinned-message state.
//!
//! Per-room pin entries projected from XEP-0470 attachment events. The
//! per-message pin marker on the wire is `<pinned xmlns='urn:waddle:pin:0'/>`
//! (see `crate::xep::xep0470`); this module owns the canonical room-level
//! list of pinned entries that the projection IQ query and live system
//! events serialize from.

use chrono::{DateTime, Utc};
use jid::BareJid;
use serde::{Deserialize, Serialize};

/// Maximum preview text length stored for a pinned message. The preview
/// is intentionally lossy — it's a projection-list affordance, not a
/// faithful render. Click-through fetches the full body via MAM.
pub const MAX_PREVIEW_LEN: usize = 280;

/// Frozen snapshot of a pinned message at pin time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinPreview {
    /// Bare JID of the message author.
    pub author_jid: BareJid,
    /// MUC nickname of the author at pin time, if known.
    pub author_nick: Option<String>,
    /// Truncated body text, capped to `MAX_PREVIEW_LEN`.
    pub text: String,
    /// Original message timestamp (delay stamp or archive ts).
    pub message_timestamp: DateTime<Utc>,
}

impl PinPreview {
    /// Build a preview, truncating the body to `MAX_PREVIEW_LEN`. The cap
    /// counts UTF-8 chars, not bytes — relies on `String::char_indices`
    /// to avoid splitting a multi-byte char.
    pub fn new(
        author_jid: BareJid,
        author_nick: Option<String>,
        body: &str,
        message_timestamp: DateTime<Utc>,
    ) -> Self {
        let text = truncate_chars(body, MAX_PREVIEW_LEN);
        Self {
            author_jid,
            author_nick,
            text,
            message_timestamp,
        }
    }
}

/// A pinned message entry held on the room actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedEntry {
    /// XEP-0359 stanza-id of the pinned message in this room. Acts as
    /// the entry's primary key — a message can be pinned at most once.
    pub target_stanza_id: String,
    /// Bare JID of the user who pinned the message.
    pub pinner_jid: BareJid,
    /// When the pin was applied.
    pub pinned_at: DateTime<Utc>,
    /// Frozen preview at pin time. Not refreshed on XEP-0308 LMC.
    pub preview: PinPreview,
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn jid(s: &str) -> BareJid {
        BareJid::from_str(s).expect("valid bare jid")
    }

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-08T12:34:56Z")
            .expect("valid rfc3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn preview_truncates_oversize_body() {
        let body = "x".repeat(MAX_PREVIEW_LEN + 50);
        let preview = PinPreview::new(jid("alice@example.com"), None, &body, ts());
        assert_eq!(preview.text.chars().count(), MAX_PREVIEW_LEN);
    }

    #[test]
    fn preview_keeps_short_body_intact() {
        let body = "hello";
        let preview = PinPreview::new(jid("alice@example.com"), Some("Alice".into()), body, ts());
        assert_eq!(preview.text, body);
        assert_eq!(preview.author_nick.as_deref(), Some("Alice"));
    }

    #[test]
    fn preview_truncation_respects_utf8_boundaries() {
        let body = "🦆".repeat(MAX_PREVIEW_LEN + 5);
        let preview = PinPreview::new(jid("alice@example.com"), None, &body, ts());
        assert_eq!(preview.text.chars().count(), MAX_PREVIEW_LEN);
        assert!(preview.text.starts_with('🦆'));
    }

    #[test]
    fn pinned_entry_roundtrips_via_serde_json() {
        let entry = PinnedEntry {
            target_stanza_id: "stanza-abc".into(),
            pinner_jid: jid("admin@example.com"),
            pinned_at: ts(),
            preview: PinPreview::new(
                jid("alice@example.com"),
                Some("Alice".into()),
                "important",
                ts(),
            ),
        };
        let serialized = serde_json::to_string(&entry).expect("serialize");
        let parsed: PinnedEntry = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(entry, parsed);
    }
}
