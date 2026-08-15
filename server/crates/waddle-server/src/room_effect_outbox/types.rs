use jid::{BareJid, FullJid};
use thiserror::Error;
use waddle_xmpp::muc::{
    DestroyPassword, DestroyReason, DestroyRecipient, MucConfigStatusCode, OccupantPresenceUpdate,
    OccupantVoiceChange, RoomEffect, RoomEffectKind, RoomEffectOrdinal, RoomLifecycleId,
    RoomRevision,
};
use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp::Voice;

use crate::db::DatabaseError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomEffectOriginInstanceId(String);
impl RoomEffectOriginInstanceId {
    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomEffectLeaseToken(String);
impl Default for RoomEffectLeaseToken {
    fn default() -> Self {
        Self::new()
    }
}
impl RoomEffectLeaseToken {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
    pub(crate) fn from_stored(value: String) -> Self {
        Self(value)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomEffectProducingNode(NodeIdentity);
impl RoomEffectProducingNode {
    pub fn from_node_identity(identity: NodeIdentity) -> Self {
        Self(identity)
    }
    pub fn node_identity(&self) -> &NodeIdentity {
        &self.0
    }
    pub(crate) fn as_db_value(&self) -> Result<String, RoomEffectOutboxError> {
        serde_json::to_string(&(&self.0.node_id, &self.0.node_epoch))
            .map_err(RoomEffectOutboxError::EncodeProducingNode)
    }
    pub(crate) fn from_db_value(value: String) -> Result<Self, RoomEffectOutboxError> {
        let (node_id, node_epoch): (String, String) = serde_json::from_str(&value)
            .map_err(|source| RoomEffectOutboxError::InvalidProducingNode { value, source })?;
        Ok(Self(NodeIdentity::new(node_id, node_epoch)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomEffectKey {
    pub lifecycle: RoomLifecycleId,
    pub revision: RoomRevision,
    pub ordinal: RoomEffectOrdinal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomEffectRow {
    pub key: RoomEffectKey,
    pub room_jid: BareJid,
    pub effect: RoomEffect,
    pub available_at_ms: i64,
    pub superseded: bool,
    pub origin_instance_id: RoomEffectOriginInstanceId,
    pub producing_node: RoomEffectProducingNode,
    pub lease_token: Option<RoomEffectLeaseToken>,
    pub leased_at_ms: Option<i64>,
    pub attempt_count: i64,
    pub last_error: Option<RoomEffectLastError>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedRoomEffect {
    pub row: RoomEffectRow,
    pub lease_token: RoomEffectLeaseToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomEffectLastError {
    Retryable,
    InfrastructureTransient,
}
impl RoomEffectLastError {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::InfrastructureTransient => "infrastructure_transient",
        }
    }
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "retryable" => Some(Self::Retryable),
            "infrastructure_transient" => Some(Self::InfrastructureTransient),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomEffectReleaseOutcome {
    Released { attempt_count: i64 },
    DeadLettered { attempt_count: i64 },
    LostLease,
}

#[derive(Debug, Error)]
pub enum RoomEffectOutboxError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("could not encode room effect payload")]
    EncodePayload(#[source] serde_json::Error),
    #[error("could not decode room effect payload")]
    DecodePayload(#[source] serde_json::Error),
    #[error("unknown room-effect kind: {0}")]
    UnknownKind(String),
    #[error("invalid room-effect stored coordinate")]
    InvalidCoordinate,
    #[error("invalid stored room JID: {0}")]
    InvalidRoomJid(String),
    #[error("invalid producing node encoding: {value}")]
    InvalidProducingNode {
        value: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("could not encode producing node")]
    EncodeProducingNode(#[source] serde_json::Error),
    #[error("invalid persisted room effect")]
    InvalidPayload,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersistedMucConfigStatusCode {
    NonPrivacyConfigurationChange,
    LoggingEnabled,
    LoggingDisabled,
}
impl From<MucConfigStatusCode> for PersistedMucConfigStatusCode {
    fn from(v: MucConfigStatusCode) -> Self {
        match v {
            MucConfigStatusCode::NonPrivacyConfigurationChange => {
                Self::NonPrivacyConfigurationChange
            }
            MucConfigStatusCode::LoggingEnabled => Self::LoggingEnabled,
            MucConfigStatusCode::LoggingDisabled => Self::LoggingDisabled,
        }
    }
}
impl From<PersistedMucConfigStatusCode> for MucConfigStatusCode {
    fn from(v: PersistedMucConfigStatusCode) -> Self {
        match v {
            PersistedMucConfigStatusCode::NonPrivacyConfigurationChange => {
                Self::NonPrivacyConfigurationChange
            }
            PersistedMucConfigStatusCode::LoggingEnabled => Self::LoggingEnabled,
            PersistedMucConfigStatusCode::LoggingDisabled => Self::LoggingDisabled,
        }
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersistedVoice {
    Voiced,
    Muted,
}
impl From<Voice> for PersistedVoice {
    fn from(v: Voice) -> Self {
        match v {
            Voice::Voiced => Self::Voiced,
            Voice::Muted => Self::Muted,
        }
    }
}
impl From<PersistedVoice> for Voice {
    fn from(v: PersistedVoice) -> Self {
        match v {
            PersistedVoice::Voiced => Self::Voiced,
            PersistedVoice::Muted => Self::Muted,
        }
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedOccupantVoiceChange {
    session: FullJid,
    voice: PersistedVoice,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PersistedRoomEffect {
    ConfigChanged {
        status_codes: Vec<PersistedMucConfigStatusCode>,
        recipients: Vec<FullJid>,
    },
    AdminSelfNotify {
        updates: Vec<OccupantPresenceUpdate>,
    },
    AdminRemainingBroadcast {
        presence_updates: Vec<OccupantPresenceUpdate>,
        voice_changes: Vec<PersistedOccupantVoiceChange>,
    },
    DestroyNotification {
        reason: Option<DestroyReason>,
        alternate_venue: Option<BareJid>,
        password: Option<DestroyPassword>,
        recipients: Vec<DestroyRecipient>,
    },
}
impl From<&RoomEffect> for PersistedRoomEffect {
    fn from(value: &RoomEffect) -> Self {
        match value {
            RoomEffect::ConfigChanged {
                status_codes,
                recipients,
            } => Self::ConfigChanged {
                status_codes: status_codes.iter().copied().map(Into::into).collect(),
                recipients: recipients.clone(),
            },
            RoomEffect::AdminSelfNotify { updates } => Self::AdminSelfNotify {
                updates: updates.clone(),
            },
            RoomEffect::AdminRemainingBroadcast {
                presence_updates,
                voice_changes,
            } => Self::AdminRemainingBroadcast {
                presence_updates: presence_updates.clone(),
                voice_changes: voice_changes
                    .iter()
                    .map(|v| PersistedOccupantVoiceChange {
                        session: v.session.clone(),
                        voice: v.voice.into(),
                    })
                    .collect(),
            },
            RoomEffect::DestroyNotification {
                reason,
                alternate_venue,
                password,
                recipients,
            } => Self::DestroyNotification {
                reason: reason.clone(),
                alternate_venue: alternate_venue.clone(),
                password: password.clone(),
                recipients: recipients.clone(),
            },
        }
    }
}
impl TryFrom<PersistedRoomEffect> for RoomEffect {
    type Error = RoomEffectOutboxError;
    fn try_from(value: PersistedRoomEffect) -> Result<Self, Self::Error> {
        Ok(match value {
            PersistedRoomEffect::ConfigChanged {
                status_codes,
                recipients,
            } => Self::ConfigChanged {
                status_codes: status_codes.into_iter().map(Into::into).collect(),
                recipients,
            },
            PersistedRoomEffect::AdminSelfNotify { updates } => Self::AdminSelfNotify { updates },
            PersistedRoomEffect::AdminRemainingBroadcast {
                presence_updates,
                voice_changes,
            } => Self::AdminRemainingBroadcast {
                presence_updates,
                voice_changes: voice_changes
                    .into_iter()
                    .map(|v| OccupantVoiceChange {
                        session: v.session,
                        voice: v.voice.into(),
                    })
                    .collect(),
            },
            PersistedRoomEffect::DestroyNotification {
                reason,
                alternate_venue,
                password,
                recipients,
            } => Self::DestroyNotification {
                reason,
                alternate_venue,
                password,
                recipients,
            },
        })
    }
}

pub(crate) fn encode_effect(effect: &RoomEffect) -> Result<String, RoomEffectOutboxError> {
    serde_json::to_string(&PersistedRoomEffect::from(effect))
        .map_err(RoomEffectOutboxError::EncodePayload)
}
pub(crate) fn decode_effect(kind: &str, value: &str) -> Result<RoomEffect, RoomEffectOutboxError> {
    let expected = RoomEffectKind::from_db_str(kind)
        .ok_or_else(|| RoomEffectOutboxError::UnknownKind(kind.to_owned()))?;
    let persisted: PersistedRoomEffect =
        serde_json::from_str(value).map_err(RoomEffectOutboxError::DecodePayload)?;
    let effect: RoomEffect =
        RoomEffect::try_from(persisted).expect("persisted room-effect conversion is infallible");
    (effect.kind() == expected)
        .then_some(effect)
        .ok_or(RoomEffectOutboxError::InvalidPayload)
}
