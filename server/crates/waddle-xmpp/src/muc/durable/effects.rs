//! Typed, durable MUC room-mutation effects (#1646).

use jid::{BareJid, FullJid};

use crate::muc::MucConfigStatusCode;
use crate::types::{Role, Voice};

use super::{RoomEffectOrdinal, RoomLifecycleId, RoomRevision};

/// A validated room nickname carried by a durable effect.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MucOccupantNick(String);

impl MucOccupantNick {
    pub fn new(value: String) -> Option<Self> {
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Human-facing XEP-0045 destroy reason. Kept distinct from an arbitrary
/// protocol payload string at the durable boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DestroyReason(String);

impl DestroyReason {
    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Password supplied for an alternate room on XEP-0045 destruction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DestroyPassword(String);

impl DestroyPassword {
    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestroyRecipient {
    pub nick: MucOccupantNick,
    pub sessions: Vec<FullJid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AdminPresenceKind {
    Banned,
    Kicked,
    AffiliationRemoved,
    MembersOnlyRemoved,
    RoleChanged(Role),
}

/// Semantic data from which an admin presence is rebuilt at drain time; this
/// deliberately does not retain a serialized stanza.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OccupantPresenceUpdate {
    pub recipient: FullJid,
    pub occupant: FullJid,
    pub nick: MucOccupantNick,
    pub kind: AdminPresenceKind,
    pub actor: Option<BareJid>,
    pub reason: Option<DestroyReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccupantVoiceChange {
    pub session: FullJid,
    pub voice: Voice,
}

/// Closed effect vocabulary persisted by the room-effect outbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomEffect {
    ConfigChanged {
        status_codes: Vec<MucConfigStatusCode>,
        recipients: Vec<FullJid>,
    },
    AdminSelfNotify {
        updates: Vec<OccupantPresenceUpdate>,
    },
    AdminRemainingBroadcast {
        presence_updates: Vec<OccupantPresenceUpdate>,
        voice_changes: Vec<OccupantVoiceChange>,
        recipients: Vec<FullJid>,
    },
    DestroyNotification {
        reason: Option<DestroyReason>,
        alternate_venue: Option<BareJid>,
        password: Option<DestroyPassword>,
        recipients: Vec<DestroyRecipient>,
    },
}

impl RoomEffect {
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::DestroyNotification { .. })
    }
    pub const fn kind(&self) -> RoomEffectKind {
        match self {
            Self::ConfigChanged { .. } => RoomEffectKind::ConfigChanged,
            Self::AdminSelfNotify { .. } => RoomEffectKind::AdminSelfNotify,
            Self::AdminRemainingBroadcast { .. } => RoomEffectKind::AdminRemainingBroadcast,
            Self::DestroyNotification { .. } => RoomEffectKind::DestroyNotification,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomEffectKind {
    ConfigChanged,
    AdminSelfNotify,
    AdminRemainingBroadcast,
    DestroyNotification,
}

impl RoomEffectKind {
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::ConfigChanged => "config_changed",
            Self::AdminSelfNotify => "admin_self_notify",
            Self::AdminRemainingBroadcast => "admin_remaining_broadcast",
            Self::DestroyNotification => "destroy_notification",
        }
    }
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "config_changed" => Some(Self::ConfigChanged),
            "admin_self_notify" => Some(Self::AdminSelfNotify),
            "admin_remaining_broadcast" => Some(Self::AdminRemainingBroadcast),
            "destroy_notification" => Some(Self::DestroyNotification),
            _ => None,
        }
    }
}

/// Initial eligibility selected by the mutation constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomEffectStagingClass {
    HandlerWindow,
    StagedConfig,
    Terminal,
}

/// A validated effect set for one room durable mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMutationEffects {
    room_jid: Option<BareJid>,
    staging: RoomEffectStagingClass,
    effects: Vec<RoomEffect>,
}

impl RoomMutationEffects {
    pub fn none() -> Self {
        Self {
            room_jid: None,
            staging: RoomEffectStagingClass::StagedConfig,
            effects: Vec::new(),
        }
    }
    pub fn config(
        room_jid: BareJid,
        status_codes: Vec<MucConfigStatusCode>,
        recipients: Vec<FullJid>,
    ) -> Self {
        Self {
            room_jid: Some(room_jid),
            staging: RoomEffectStagingClass::StagedConfig,
            effects: vec![RoomEffect::ConfigChanged {
                status_codes,
                recipients,
            }],
        }
    }
    pub fn admin(
        room_jid: BareJid,
        self_updates: Vec<OccupantPresenceUpdate>,
        remaining_updates: Vec<OccupantPresenceUpdate>,
        voice_changes: Vec<OccupantVoiceChange>,
        recipients: Vec<FullJid>,
    ) -> Self {
        Self {
            room_jid: Some(room_jid),
            staging: RoomEffectStagingClass::HandlerWindow,
            effects: vec![
                RoomEffect::AdminSelfNotify {
                    updates: self_updates,
                },
                RoomEffect::AdminRemainingBroadcast {
                    presence_updates: remaining_updates,
                    voice_changes,
                    recipients,
                },
            ],
        }
    }
    pub fn members_only_enforcement(
        room_jid: BareJid,
        self_updates: Vec<OccupantPresenceUpdate>,
        status_codes: Vec<MucConfigStatusCode>,
        recipients: Vec<FullJid>,
    ) -> Self {
        Self {
            room_jid: Some(room_jid),
            staging: RoomEffectStagingClass::HandlerWindow,
            effects: vec![
                RoomEffect::AdminSelfNotify {
                    updates: self_updates,
                },
                RoomEffect::ConfigChanged {
                    status_codes,
                    recipients,
                },
            ],
        }
    }
    pub fn destroy(
        room_jid: BareJid,
        reason: Option<DestroyReason>,
        alternate_venue: Option<BareJid>,
        password: Option<DestroyPassword>,
        recipients: Vec<DestroyRecipient>,
    ) -> Self {
        Self {
            room_jid: Some(room_jid),
            staging: RoomEffectStagingClass::Terminal,
            effects: vec![RoomEffect::DestroyNotification {
                reason,
                alternate_venue,
                password,
                recipients,
            }],
        }
    }
    pub fn room_jid(&self) -> Option<&BareJid> {
        self.room_jid.as_ref()
    }
    pub const fn staging(&self) -> RoomEffectStagingClass {
        self.staging
    }
    pub fn effects(&self) -> &[RoomEffect] {
        &self.effects
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomEffectReservation {
    pub lifecycle: RoomLifecycleId,
    pub revision: RoomRevision,
    pub ordinals: Vec<RoomEffectOrdinal>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    #[test]
    fn kinds_round_trip() {
        for kind in [
            RoomEffectKind::ConfigChanged,
            RoomEffectKind::AdminSelfNotify,
            RoomEffectKind::AdminRemainingBroadcast,
            RoomEffectKind::DestroyNotification,
        ] {
            assert_eq!(RoomEffectKind::from_db_str(kind.as_db_str()), Some(kind));
        }
        assert_eq!(RoomEffectKind::from_db_str("nope"), None);
    }
    #[test]
    fn constructors_fix_effect_pairing_and_stage() {
        let room = BareJid::from_str("room@example.test").unwrap();
        assert!(RoomMutationEffects::none().effects().is_empty());
        let config = RoomMutationEffects::config(
            room,
            vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            Vec::new(),
        );
        assert_eq!(config.staging(), RoomEffectStagingClass::StagedConfig);
        assert!(matches!(
            config.effects()[0],
            RoomEffect::ConfigChanged { .. }
        ));
    }
}
