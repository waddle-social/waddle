//! [`CallCorrelationId`] — the privacy-safe join key for call telemetry.
//!
//! A call is observed in three places that never share a transport:
//! the browser (Faro events), the XMPP server (Jingle call-setup
//! spans/logs), and the inbound LiveKit webhook. The one identifier
//! all three already possess is the **LiveKit room name** — Waddle's
//! [`crate::CallId`]. No new XMPP wire element is needed (and per the
//! repo's XEP-conformance rule, none may be invented when an existing
//! identifier suffices).
//!
//! The raw room name cannot be used as the correlation attribute: for
//! 1:1 calls it is `<initiator-bare-jid>::<sid>` and for Muji calls it
//! is the MUC room JID — both carry user/room identity. So the shared
//! key is a truncated SHA-256 digest of the room name: stable across
//! the three vantage points, non-reversible without already knowing
//! the room name, and short enough to be cheap on every log line.
//!
//! The chat client derives the identical value from the room name it
//! receives in the issued LiveKit transport (see
//! `chat/src/lib/calls/call-correlation.ts`); the two implementations
//! must stay byte-for-byte compatible — lowercase hex of the first
//! [`CORRELATION_ID_HEX_LEN`] / 2 digest bytes.

use sha2::{Digest, Sha256};

use crate::call::CallId;

/// Hex characters kept from the SHA-256 digest. 16 hex chars = 64
/// bits: collision-free in practice for the number of concurrent
/// calls a deployment sees, and short enough to skim in a log line.
pub const CORRELATION_ID_HEX_LEN: usize = 16;

/// Bounded, non-PII correlation key for one call, shared by the
/// client, the server call-setup path, and the LiveKit webhook.
///
/// Deliberately *not* a metric attribute: it is high-cardinality by
/// construction and belongs on spans and log lines only.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallCorrelationId(String);

impl CallCorrelationId {
    /// Derive the correlation id for `call_id` (the LiveKit room name).
    pub fn for_call(call_id: &CallId) -> Self {
        Self::for_room_name(call_id.as_str())
    }

    /// Derive the correlation id straight from a LiveKit room name.
    ///
    /// The webhook path needs this: LiveKit reports `room.name` as a
    /// plain string, and `room_finished` for a 1:1 call carries a name
    /// that is not a valid [`CallId`] target for every caller. Hashing
    /// the raw name keeps the key identical either way.
    pub fn for_room_name(room_name: &str) -> Self {
        let digest = Sha256::digest(room_name.as_bytes());
        let mut hex = String::with_capacity(CORRELATION_ID_HEX_LEN);
        for byte in digest.iter().take(CORRELATION_ID_HEX_LEN / 2) {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CallCorrelationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_id_is_stable_for_the_same_room_name() {
        let call = CallId::new("general@muc.example.com").expect("valid call id");
        assert_eq!(
            CallCorrelationId::for_call(&call),
            CallCorrelationId::for_call(&call)
        );
    }

    #[test]
    fn correlation_id_differs_between_rooms() {
        let a = CallId::new("general@muc.example.com").expect("valid call id");
        let b = CallId::new("random@muc.example.com").expect("valid call id");
        assert_ne!(
            CallCorrelationId::for_call(&a),
            CallCorrelationId::for_call(&b)
        );
    }

    #[test]
    fn correlation_id_is_bounded_lowercase_hex() {
        let call = CallId::new("alice@example.com::dm-1128").expect("valid call id");
        let id = CallCorrelationId::for_call(&call);
        assert_eq!(id.as_str().len(), CORRELATION_ID_HEX_LEN);
        assert!(
            id.as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{id}"
        );
    }

    #[test]
    fn correlation_id_leaks_no_jid_substring() {
        let call = CallId::new("alice@example.com::dm-1128").expect("valid call id");
        let id = CallCorrelationId::for_call(&call);
        assert!(!id.as_str().contains("alice"), "{id}");
        assert!(!id.as_str().contains("example"), "{id}");
    }

    /// Pins the exact digest the chat client must reproduce. If this
    /// value changes, `chat/src/lib/calls/call-correlation.ts` and its
    /// test must change with it or client and server telemetry stop
    /// joining.
    #[test]
    fn correlation_id_matches_the_pinned_cross_client_vector() {
        // sha256("general@muc.example.com") =
        // ba2798ebd1a58db8eb039d63cb6b8d2b1c33ac932d0ca0d55f7378b88551caaf
        assert_eq!(
            CallCorrelationId::for_room_name("general@muc.example.com").as_str(),
            "ba2798ebd1a58db8",
        );
    }

    #[test]
    fn room_name_and_call_id_derivations_agree() {
        let call = CallId::new("general@muc.example.com").expect("valid call id");
        assert_eq!(
            CallCorrelationId::for_call(&call),
            CallCorrelationId::for_room_name("general@muc.example.com")
        );
    }
}
