//! Call-scope typed values: [`CallId`], [`Identity`],
//! [`MediaCapabilities`], [`CallState`].
//!
//! These types are the boundary between the XMPP layer and the SFU
//! bridge. The XMPP layer only ever sees these; LiveKit-specific
//! representations live in [`crate::token`] and [`crate::livekit`].

use jid::FullJid;
use waddle_xmpp_core::types::Voice;

use crate::error::SfuError;

/// Opaque LiveKit room name. For 1:1 calls this is the Jingle `sid`
/// (scoped by the initiator's bare JID, see `scoped_call_id`); for
/// MUC group calls it is the MUC room JID itself, as set by the
/// XEP-0272 Muji branch of the Jingle handler — every occupant who
/// joins the call lands in the SAME LiveKit room because the Muji
/// `<jingle/>` carries `<muji room='…'/>` and the room JID maps
/// directly onto the SFU `CallId`.
///
/// LiveKit accepts arbitrary UTF-8 room names but Waddle constrains
/// them to a printable ASCII subset (alphanumerics + `-`, `_`, `:`)
/// to keep them safe to embed in stanzas and URLs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallId(String);

impl CallId {
    pub fn new(value: impl Into<String>) -> Result<Self, SfuError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 {
            return Err(SfuError::InvalidCallId(value));
        }
        // The Jingle handler namespaces call ids by the initiator's
        // bare JID (`<localpart>@<domain>::<sid>`) to prevent room
        // collisions. The whitelist permits JID-safe characters plus
        // the `::` separator chars.
        let valid = value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '@'));
        if !valid {
            return Err(SfuError::InvalidCallId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CallId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// LiveKit participant identity. Always derived from a real
/// [`FullJid`] so participant ↔ JID is a 1:1 mapping in the issued
/// JWT's `sub` claim.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Identity(FullJid);

impl Identity {
    pub fn from_jid(jid: FullJid) -> Self {
        Self(jid)
    }

    pub fn as_jid(&self) -> &FullJid {
        &self.0
    }

    /// Stringified form used as the LiveKit participant identity and
    /// as the second segment of the TURN time-limited username.
    pub fn as_livekit_identity(&self) -> String {
        self.0.to_string()
    }
}

/// Per-participant grants. Translated 1:1 into the LiveKit `video`
/// grant in the issued JWT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaCapabilities {
    pub can_publish: bool,
    pub can_subscribe: bool,
    pub can_publish_data: bool,
}

impl MediaCapabilities {
    /// Grants for a 1:1 call peer. Both sides of a direct call are
    /// symmetric, mutually-consenting participants, so each receives
    /// full publish + subscribe rights.
    pub fn direct_call_peer() -> Self {
        Self {
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
        }
    }

    /// Grants for a MUC (XEP-0272 Muji) call occupant, derived from
    /// the occupant's XEP-0045 voice. An occupant without voice may
    /// listen but not publish, exactly mirroring the text-broadcast
    /// rule (§7.5) — the two authorization models share
    /// [`waddle_xmpp_core::types::Role::voice`] as their single
    /// predicate, so media and text can never disagree.
    ///
    /// Affiliation is not consulted here: it is an input to role
    /// assignment, which the room actor already resolves.
    pub fn from_muc_voice(voice: Voice) -> Self {
        match voice {
            Voice::Voiced => Self {
                can_publish: true,
                can_subscribe: true,
                can_publish_data: true,
            },
            Voice::Muted => Self::listen_only(),
        }
    }

    /// Subscribe-only grants. The fail-closed floor for any MUC mint
    /// that cannot establish the caller's voice.
    pub fn listen_only() -> Self {
        Self {
            can_publish: false,
            can_subscribe: true,
            can_publish_data: false,
        }
    }

    /// True when these grants allow no publishing of any kind.
    pub fn is_listen_only(&self) -> bool {
        !self.can_publish && !self.can_publish_data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Voice → grant mapping. An occupant with voice may publish; one
    /// without may only listen. (The role+moderation → voice policy
    /// itself is pinned in `waddle-xmpp-core`'s `Role::voice` tests —
    /// it is deliberately not duplicated here.)
    #[test]
    fn voice_grant_table() {
        let voiced = MediaCapabilities::from_muc_voice(Voice::Voiced);
        assert!(voiced.can_publish && voiced.can_publish_data && voiced.can_subscribe);
        assert!(!voiced.is_listen_only());

        let muted = MediaCapabilities::from_muc_voice(Voice::Muted);
        assert!(!muted.can_publish);
        assert!(!muted.can_publish_data);
        assert!(muted.can_subscribe, "an occupant without voice may listen");
        assert!(muted.is_listen_only());
        assert_eq!(muted, MediaCapabilities::listen_only());
    }

    #[test]
    fn direct_call_peers_get_full_grants() {
        let caps = MediaCapabilities::direct_call_peer();
        assert!(caps.can_publish && caps.can_subscribe && caps.can_publish_data);
        assert!(!caps.is_listen_only());
    }
}

/// Result of [`crate::SfuService::unregister_call_participant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// The call still has at least one other participant after
    /// removing the caller.
    Active { remaining: usize },
    /// The caller was the last participant; the call entry has been
    /// removed from the registry. Clients drop their XEP-0272 Muji
    /// presence advertisement on this transition (per §Leaving —
    /// absence of `<muji/>` is the leave marker).
    Ended,
}
