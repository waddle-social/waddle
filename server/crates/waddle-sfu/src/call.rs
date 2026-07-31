//! Call-scope typed values: [`CallId`], [`Identity`], SIDs,
//! [`MediaCapabilities`], and teardown dispositions.
//!
//! These types are the boundary between the XMPP layer and the SFU
//! bridge. The XMPP layer only ever sees these; LiveKit-specific
//! representations live in [`crate::token`] and [`crate::livekit`].

use jid::FullJid;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use waddle_xmpp_core::types::Voice;

use crate::error::SfuError;

const MAX_SID_LEN: usize = 256;

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

/// Opaque LiveKit room sid. Distinct from [`CallId`]: LiveKit reuses a
/// room's human name forever, but each concrete room incarnation gets
/// a fresh sid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomSid(String);

impl RoomSid {
    pub fn new(value: impl Into<String>) -> Result<Self, SfuError> {
        let value = value.into();
        if !is_valid_printable_ascii_sid(&value) {
            return Err(SfuError::InvalidRoomSid);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for RoomSid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RoomSid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl std::fmt::Display for RoomSid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque LiveKit participant sid for one participant incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParticipantSid(String);

impl ParticipantSid {
    pub fn new(value: impl Into<String>) -> Result<Self, SfuError> {
        let value = value.into();
        if !is_valid_printable_ascii_sid(&value) {
            return Err(SfuError::InvalidParticipantSid);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ParticipantSid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ParticipantSid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

impl std::fmt::Display for ParticipantSid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Monotonically increasing generation for one [`CallId`]'s concrete
/// room incarnations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallGeneration(u64);

impl CallGeneration {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for CallGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// SIDs observed on a webhook/admin event. Presence of either sid is
/// optional so callers can remain backward-compatible with older
/// envelopes and call sites that do not yet propagate them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObservedCallSids {
    pub room_sid: Option<RoomSid>,
    pub participant_sid: Option<ParticipantSid>,
}

impl ObservedCallSids {
    pub fn new(room_sid: Option<RoomSid>, participant_sid: Option<ParticipantSid>) -> Self {
        Self {
            room_sid,
            participant_sid,
        }
    }

    pub fn none() -> Self {
        Self {
            room_sid: None,
            participant_sid: None,
        }
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
    pub fn direct_call_peer() -> Self {
        Self {
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
        }
    }

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

    pub fn listen_only() -> Self {
        Self {
            can_publish: false,
            can_subscribe: true,
            can_publish_data: false,
        }
    }

    pub fn is_listen_only(&self) -> bool {
        !self.can_publish && !self.can_publish_data
    }
}

/// Result of [`crate::SfuService::unregister_call_participant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Active { remaining: usize },
    Ended,
}

/// Result of a teardown path guarded by observed LiveKit sids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownDisposition {
    Applied(CallState),
    StaleSid,
}

/// Result of non-destructive SID observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidObservationDisposition {
    Applied,
    StaleSid,
}

fn is_valid_printable_ascii_sid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SID_LEN
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn room_sid_requires_printable_ascii() {
        assert!(RoomSid::new("RM_123").is_ok());
        assert!(matches!(RoomSid::new(""), Err(SfuError::InvalidRoomSid)));
        assert!(matches!(
            RoomSid::new("RM_\n123"),
            Err(SfuError::InvalidRoomSid)
        ));
        assert!(RoomSid::new("RM 123").is_ok(), "space is printable ASCII");
    }

    #[test]
    fn participant_sid_requires_printable_ascii() {
        assert!(ParticipantSid::new("PA_123").is_ok());
        assert!(matches!(
            ParticipantSid::new("π"),
            Err(SfuError::InvalidParticipantSid)
        ));
    }
}
