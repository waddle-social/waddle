//! Typed, durable MUC room-mutation effects (#1646).

use jid::{BareJid, FullJid};

use crate::muc::MucConfigStatusCode;
use crate::types::{Affiliation, Role, Voice};

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
    pub is_self: bool,
    pub occupant: FullJid,
    pub nick: MucOccupantNick,
    /// Identity snapshot needed to rebuild the exact MUC presence after the
    /// affected occupant has already left the live room actor.
    pub occupant_bare_jid: BareJid,
    pub disclosed_real_jid: Option<FullJid>,
    pub affiliation: Affiliation,
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
        removed_sessions: Vec<FullJid>,
        voice_changes: Vec<OccupantVoiceChange>,
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
///
/// Load-bearing invariant: within one mutation's effect set, the recipient
/// sets of distinct ordinals are DISJOINT (self-frames vs remaining
/// broadcast vs post-removal config audiences). The server enforces the
/// drain-side bypass with an exact recipient-overlap check against earlier
/// retained rows, and that check is only truthful if these typed effect
/// payloads continue to describe the full delivery audience for each ordinal.
/// The inline drain relies on this: it may deliver ordinal k while ordinal
/// k-1's lease is still retained for the response batch, so a shared
/// recipient could otherwise observe a FIFO inversion across a crash gap.
/// Every constructor below must preserve disjointness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomMutationEffects {
    room_jid: Option<BareJid>,
    staging: RoomEffectStagingClass,
    effects: Vec<RoomEffect>,
    superseding_reservation: Option<RoomEffectReservation>,
}

impl RoomMutationEffects {
    pub fn none() -> Self {
        Self {
            room_jid: None,
            staging: RoomEffectStagingClass::StagedConfig,
            effects: Vec::new(),
            superseding_reservation: None,
        }
    }
    pub fn none_superseding(reservation: RoomEffectReservation) -> Self {
        Self {
            superseding_reservation: Some(reservation),
            ..Self::none()
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
            superseding_reservation: None,
        }
    }
    pub fn admin(
        room_jid: BareJid,
        self_updates: Vec<OccupantPresenceUpdate>,
        remaining_updates: Vec<OccupantPresenceUpdate>,
        removed_sessions: Vec<FullJid>,
        voice_changes: Vec<OccupantVoiceChange>,
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
                    removed_sessions,
                    voice_changes,
                },
            ],
            superseding_reservation: None,
        }
    }
    pub fn members_only_enforcement(
        room_jid: BareJid,
        self_updates: Vec<OccupantPresenceUpdate>,
        remaining_updates: Vec<OccupantPresenceUpdate>,
        removed_sessions: Vec<FullJid>,
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
                RoomEffect::AdminRemainingBroadcast {
                    presence_updates: remaining_updates,
                    removed_sessions,
                    voice_changes: Vec::new(),
                },
                RoomEffect::ConfigChanged {
                    status_codes,
                    recipients,
                },
            ],
            superseding_reservation: None,
        }
    }
    pub fn with_superseding_reservation(mut self, reservation: RoomEffectReservation) -> Self {
        self.superseding_reservation = Some(reservation);
        self
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
            superseding_reservation: None,
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
    pub fn superseding_reservation(&self) -> Option<&RoomEffectReservation> {
        self.superseding_reservation.as_ref()
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
    use std::collections::HashSet;
    use std::str::FromStr;

    fn effect_recipients(effect: &RoomEffect) -> HashSet<FullJid> {
        match effect {
            RoomEffect::ConfigChanged { recipients, .. } => recipients.iter().cloned().collect(),
            RoomEffect::AdminSelfNotify { updates } => updates
                .iter()
                .map(|update| update.recipient.clone())
                .collect(),
            RoomEffect::AdminRemainingBroadcast {
                presence_updates, ..
            } => presence_updates
                .iter()
                .map(|update| update.recipient.clone())
                .collect(),
            RoomEffect::DestroyNotification { recipients, .. } => recipients
                .iter()
                .flat_map(|recipient| recipient.sessions.iter().cloned())
                .collect(),
        }
    }

    fn effects_are_pairwise_recipient_disjoint(effects: &[RoomEffect]) -> bool {
        let mut seen = HashSet::new();
        for effect in effects {
            for recipient in effect_recipients(effect) {
                if !seen.insert(recipient) {
                    return false;
                }
            }
        }
        true
    }

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

    #[test]
    fn handler_window_constructors_keep_ordinals_recipient_disjoint() {
        let room = BareJid::from_str("room@example.test").unwrap();
        let alice = FullJid::from_str("alice@example.test/phone").unwrap();
        let bob = FullJid::from_str("bob@example.test/laptop").unwrap();
        let carol = FullJid::from_str("carol@example.test/tablet").unwrap();

        let admin = RoomMutationEffects::admin(
            room.clone(),
            vec![OccupantPresenceUpdate {
                recipient: alice.clone(),
                is_self: true,
                occupant: FullJid::from_str("room@example.test/alice").unwrap(),
                nick: MucOccupantNick::new("alice".to_owned()).unwrap(),
                occupant_bare_jid: BareJid::from_str("alice@example.test").unwrap(),
                disclosed_real_jid: Some(alice.clone()),
                affiliation: Affiliation::Member,
                kind: AdminPresenceKind::Kicked,
                actor: None,
                reason: None,
            }],
            vec![OccupantPresenceUpdate {
                recipient: bob.clone(),
                is_self: false,
                occupant: FullJid::from_str("room@example.test/alice").unwrap(),
                nick: MucOccupantNick::new("alice".to_owned()).unwrap(),
                occupant_bare_jid: BareJid::from_str("alice@example.test").unwrap(),
                disclosed_real_jid: Some(alice.clone()),
                affiliation: Affiliation::Member,
                kind: AdminPresenceKind::RoleChanged(Role::Participant),
                actor: None,
                reason: None,
            }],
            vec![carol.clone()],
            Vec::new(),
        );
        assert!(effects_are_pairwise_recipient_disjoint(admin.effects()));

        let members_only = RoomMutationEffects::members_only_enforcement(
            room,
            vec![OccupantPresenceUpdate {
                recipient: alice.clone(),
                is_self: true,
                occupant: FullJid::from_str("room@example.test/alice").unwrap(),
                nick: MucOccupantNick::new("alice".to_owned()).unwrap(),
                occupant_bare_jid: BareJid::from_str("alice@example.test").unwrap(),
                disclosed_real_jid: Some(alice),
                affiliation: Affiliation::Member,
                kind: AdminPresenceKind::MembersOnlyRemoved,
                actor: None,
                reason: None,
            }],
            vec![OccupantPresenceUpdate {
                recipient: bob.clone(),
                is_self: false,
                occupant: FullJid::from_str("room@example.test/alice").unwrap(),
                nick: MucOccupantNick::new("alice".to_owned()).unwrap(),
                occupant_bare_jid: BareJid::from_str("alice@example.test").unwrap(),
                disclosed_real_jid: None,
                affiliation: Affiliation::None,
                kind: AdminPresenceKind::MembersOnlyRemoved,
                actor: None,
                reason: None,
            }],
            vec![carol.clone()],
            vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            vec![carol],
        );
        assert!(effects_are_pairwise_recipient_disjoint(
            members_only.effects()
        ));
    }
}
