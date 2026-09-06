//! Typed room mutations and delivery work retained by an ingress plan.
use jid::BareJid;
use waddle_xmpp::{
    inbox::{storage::GroupchatNotificationRecovery, InboxEntry},
    mam::{ArchiveExpectation, ArchivedMessage},
    muc::{pin::PinStateChange, RoomClaimFenceContext, SubjectState},
};
use xmpp_parsers::message::Message;

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
        owner: BareJid,
        entry: Box<InboxEntry>,
        is_recipient: bool,
        recovery: Option<GroupchatNotificationRecovery>,
    },
}

#[derive(Debug, Clone)]
pub enum RoomActorMutation {
    SetSubject {
        claim_fence: Option<RoomClaimFenceContext>,
        subject: SubjectState,
    },
    ApplyPin {
        claim_fence: Option<RoomClaimFenceContext>,
        change: PinStateChange,
    },
}

#[derive(Debug, Clone)]
pub enum ExternalRoomEffect {
    /// Observer hooks may invoke host mutations, so unlike enrichment they run only after commit.
    ObserveRoomMessage {
        room: BareJid,
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
        recovery: Option<GroupchatNotificationRecovery>,
    },
    #[cfg(feature = "clustering")]
    RelayMucProxy {
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
        DurableRoomEffect::ProjectGroupchatInbox { entry, .. } => {
            after_archive(&entry.partner, &entry.last_stanza_id)
        }
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
        ExternalRoomEffect::RoomActorMutation { .. } => Vec::new(),
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

fn message_dependencies(room: &BareJid, message: &Message) -> Vec<super::PlanEffectDependency> {
    super::super::groupchat_archive::extract_room_stanza_id(message, room)
        .map(|id| after_archive(room, &id))
        .into_iter()
        .collect()
}
