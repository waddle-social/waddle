//! Typed room mutations and delivery work retained by an ingress plan.
#[cfg(feature = "clustering")]
pub use super::super::{OrderedRelayRouteOrigin, OrderedRelayRouteOriginKind};
use jid::BareJid;
use waddle_xmpp::{
    inbox::{storage::GroupchatNotificationRecoveryKey, InboxEntry},
    mam::{ArchiveExpectation, ArchivedMessage},
    muc::{pin::PinStateChange, RoomClaimFenceContext, SubjectState},
};
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::message::Message;

/// Recovery data captured before ingress alias resolution assigns the
/// canonical message key. It must be materialized as a
/// `GroupchatNotificationRecovery` only inside the ingress transaction.
#[derive(Debug, Clone)]
pub struct PlannedGroupchatNotificationRecovery {
    pub key: GroupchatNotificationRecoveryKey,
    pub sender_jid: jid::Jid,
    pub is_live_occupant: bool,
    pub room_members_only: bool,
    pub sender_can_broadcast_channel_mention: bool,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub enum RoomFenceRequirement {
    Unfenced,
    Guarded(RoomClaimFenceContext),
}

#[derive(Debug, Clone)]
pub enum DurableRoomEffect {
    ArchiveGroupchat {
        room: BareJid,
        message: Box<ArchivedMessage>,
        fence: RoomFenceRequirement,
        archive_expectation: ArchiveExpectation,
    },
    ProjectGroupchatInbox {
        archive_stanza_id: StanzaId,
        owner: BareJid,
        entry: Box<InboxEntry>,
        is_recipient: bool,
        recovery: Option<PlannedGroupchatNotificationRecovery>,
    },
}

#[derive(Debug, Clone)]
pub enum RoomActorMutation {
    SetSubject {
        claim_fence: Option<RoomClaimFenceContext>,
        subject: SubjectState,
        rejection_reply: Box<Message>,
    },
    ApplyPin {
        claim_fence: Option<RoomClaimFenceContext>,
        change: PinStateChange,
    },
}

#[derive(Debug, Clone)]
pub enum ExternalRoomEffect {
    /// A system archive whose content is true only after the room pin commits.
    ArchiveAfterPin {
        room: BareJid,
        message: Box<ArchivedMessage>,
        fence: RoomFenceRequirement,
        archive_expectation: ArchiveExpectation,
    },
    /// Observer hooks may invoke host mutations, so unlike enrichment they run only after commit.
    ObserveRoomMessage {
        room: BareJid,
        plugin: waddle_extensions::PluginId,
        message: Box<Message>,
        requester: BareJid,
        sender: jid::FullJid,
        error_request: Box<Message>,
    },
    RoomActorMutation {
        room: BareJid,
        mutation: RoomActorMutation,
    },
    NotificationCandidate {
        owner: BareJid,
        room: BareJid,
        archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId,
        /// None completes recovery for a candidate suppressed by the planning-time gate.
        candidate: Option<Box<crate::notification_outbox::NotificationCandidate>>,
        recovery: Option<PlannedGroupchatNotificationRecovery>,
    },
    #[cfg(feature = "clustering")]
    RelayMucProxy {
        admission: Option<crate::ingress::identity::IngressRelayAdmission>,
        room: BareJid,
        stanza: Box<waddle_xmpp::Stanza>,
        kind: crate::clustering::ordered_relay::OrderedRelayMucProxyKind,
        muc_origin: crate::clustering::ordered_relay::MucProxyOrigin,
        origin: super::super::OrderedRelayRouteOrigin,
        reflect_replies_to_sender: bool,
    },
}

pub(in super::super) fn planned_durable(effect: DurableRoomEffect) -> super::PlannedEffect {
    let dependency = match &effect {
        DurableRoomEffect::ArchiveGroupchat { room, message, .. } => {
            after_archive(room, &message.id)
        }
        DurableRoomEffect::ProjectGroupchatInbox {
            entry,
            archive_stanza_id,
            ..
        } => super::PlanEffectDependency::AfterArchive {
            archive: entry.partner.clone(),
            minted: archive_stanza_id.clone(),
        },
    };
    super::PlannedEffect::new(super::Effect::Durable(super::DurableEffect::Room(effect)))
        .with_dependency(dependency)
}

pub(in super::super) fn external(
    deps: &super::super::Deps<'_>,
    effect: ExternalRoomEffect,
    policy: super::PlanSuppressionPolicy,
) {
    let dependencies = match &effect {
        ExternalRoomEffect::ObserveRoomMessage { room, message, .. } => {
            message_dependencies(room, message)
        }
        #[cfg(feature = "clustering")]
        ExternalRoomEffect::RelayMucProxy { room, stanza, .. } => match stanza.as_ref() {
            waddle_xmpp::Stanza::Message(message) => message_dependencies(room, message),
            _ => Vec::new(),
        },
        ExternalRoomEffect::RoomActorMutation { .. }
        | ExternalRoomEffect::ArchiveAfterPin { .. } => Vec::new(),
        ExternalRoomEffect::NotificationCandidate {
            room,
            archive_stanza_id,
            ..
        } => vec![super::PlanEffectDependency::AfterArchive {
            archive: room.clone(),
            minted: archive_stanza_id.clone(),
        }],
    };
    let mut planned =
        super::PlannedEffect::new(super::Effect::External(super::ExternalEffect::Room(effect)))
            .with_suppression(policy);
    planned.dependencies = dependencies;
    deps.effects.record(planned);
}

fn after_archive(room: &BareJid, id: &str) -> super::PlanEffectDependency {
    super::PlanEffectDependency::AfterArchive {
        archive: room.clone(),
        minted: waddle_xmpp_core::xep0359::StanzaId::new(id, jid::Jid::from(room.clone())),
    }
}

pub(crate) fn message_dependencies(
    room: &BareJid,
    message: &Message,
) -> Vec<super::PlanEffectDependency> {
    super::super::groupchat_archive::extract_room_stanza_id(message, room)
        .map(|id| after_archive(room, &id))
        .into_iter()
        .collect()
}
