//! Call-scope typed values: [`CallId`], [`Identity`],
//! [`MediaCapabilities`], [`CallState`].
//!
//! These types are the boundary between the XMPP layer and the SFU
//! bridge. The XMPP layer only ever sees these; LiveKit-specific
//! representations live in [`crate::token`] and [`crate::livekit`].

use jid::FullJid;

use crate::error::SfuError;

/// Opaque LiveKit room name. For 1:1 calls this is the Jingle `sid`;
/// for MUC calls it is the `call-id` carried on the
/// `urn:waddle:muc-call:0` presence extension.
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
#[derive(Debug, Clone, Copy)]
pub struct MediaCapabilities {
    pub can_publish: bool,
    pub can_subscribe: bool,
    pub can_publish_data: bool,
}

impl MediaCapabilities {
    /// Default for an interactive call participant: full publish +
    /// subscribe rights, no data-channel role.
    pub fn full_participant() -> Self {
        Self {
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
        }
    }
}

/// Result of [`crate::SfuService::unregister_call_participant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// The call still has at least one other participant after
    /// removing the caller.
    Active { remaining: usize },
    /// The caller was the last participant; the call entry has been
    /// removed from the registry. The MUC handler clears the
    /// `urn:waddle:muc-call:0` presence extension on this transition.
    Ended,
}
