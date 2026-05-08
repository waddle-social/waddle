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
use waddle_xmpp_core::xep0359::StanzaId;

/// Maximum preview text length stored for a pinned message. The preview
/// is intentionally lossy — it's a projection-list affordance, not a
/// faithful render. Click-through fetches the full body via MAM.
pub const MAX_PREVIEW_LEN: usize = 280;

/// Per-room pin permission policy (#415). Drives the `MucPinHandler`
/// authorization gate via `RoomConfig.pin_permission`. The value is
/// configurable through the standard XEP-0045 owner-config form using
/// the `urn:waddle:roomconfig:pinpermission` field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PinPermission {
    /// Only Owners and Admins may pin/unpin (default).
    #[default]
    AdminsOnly,
    /// Any current room occupant may pin/unpin.
    Anyone,
}

impl PinPermission {
    /// Wire value used in the `urn:waddle:roomconfig:pinpermission`
    /// data-form field. Stable across Waddle versions.
    pub fn as_form_value(self) -> &'static str {
        match self {
            PinPermission::AdminsOnly => "admins-only",
            PinPermission::Anyone => "anyone",
        }
    }

    /// Parse from the data-form `<value>` text. Returns `None` for
    /// unknown / malformed values; callers fall back to the default.
    pub fn from_form_value(value: &str) -> Option<Self> {
        match value {
            "admins-only" => Some(PinPermission::AdminsOnly),
            "anyone" => Some(PinPermission::Anyone),
            _ => None,
        }
    }
}

/// Maximum pinned entries kept on a single room. When this cap is
/// exceeded by a new pin (not a replacement), the oldest entry is
/// evicted. Bounds room actor memory so admin pin-spam can't exhaust
/// resources. Per the design grill (#414 Q9), v1 ships effectively
/// unbounded — `1_000` is the documented "future projection node
/// max_items" target and serves here as the in-memory ceiling.
pub const MAX_PINNED_ENTRIES: usize = 1_000;

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

/// A pin state mutation to apply to a room actor's pin list. The
/// actor consumes this directly via the `ApplyPin` message — Pin
/// carries a fully resolved [`PinnedEntry`] (preview already populated
/// from MAM by the interpreter), Unpin carries just the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinStateChange {
    /// Add or replace a pin entry (newest pin first per upsert).
    Pin(PinnedEntry),
    /// Remove the pin entry matching `target_stanza_id`, if any.
    Unpin {
        /// Typed XEP-0359 stanza-id of the message whose pin is being cleared.
        target_stanza_id: StanzaId,
    },
}

/// A pin/unpin request carried by [`crate::protocol::event::OutboundEvent::ApplyPinChange`]
/// from the chain handler to the interpreter. Distinct from
/// [`PinStateChange`] because the chain is **synchronous** and cannot
/// resolve the target message preview from MAM — the interpreter does
/// the async lookup, builds the `PinnedEntry`, and only then hands a
/// resolved [`PinStateChange`] to the room actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinChangeRequest {
    /// Pin a message. The interpreter resolves the preview from MAM
    /// at apply time so the stored preview reflects the *target*
    /// message author + body + original timestamp, not the pinner or
    /// pin-marker dispatch time.
    Pin {
        target_stanza_id: StanzaId,
        pinner_jid: BareJid,
        pinner_nick: String,
        pinned_at: DateTime<Utc>,
    },
    /// Unpin a message. Carries the pinner identity so the broadcast
    /// system message can attribute the action; `reason` is set to
    /// `"retracted"` for the XEP-0424 retraction cascade.
    Unpin {
        target_stanza_id: StanzaId,
        pinner_jid: BareJid,
        pinner_nick: String,
        reason: Option<String>,
    },
}

impl PinChangeRequest {
    /// The target stanza-id this request mutates.
    pub fn target_stanza_id(&self) -> &StanzaId {
        match self {
            PinChangeRequest::Pin {
                target_stanza_id, ..
            }
            | PinChangeRequest::Unpin {
                target_stanza_id, ..
            } => target_stanza_id,
        }
    }
}

/// A pinned message entry held on the room actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedEntry {
    /// Typed XEP-0359 stanza-id of the pinned message in this room.
    /// Acts as the entry's primary key — a message can be pinned at
    /// most once.
    pub target_stanza_id: StanzaId,
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

    fn stanza(id: &str) -> StanzaId {
        StanzaId::new(id.to_owned(), jid::Jid::from(jid("room@conf.example")))
    }

    #[test]
    fn pinned_entry_roundtrips_via_serde_json() {
        let entry = PinnedEntry {
            target_stanza_id: stanza("stanza-abc"),
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
