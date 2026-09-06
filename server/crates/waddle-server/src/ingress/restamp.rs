//! Pure replacement of provisional archive identities with committed XEP-0359 IDs.
mod intents;
#[cfg(test)]
mod tests;

use crate::server::routes::interpret::effects::{
    delivery::{ExternalDeliveryEffect, PreparedOfflineNotification},
    direct::{DurableDirectEffect, ExternalDirectEffect},
    room::{DurableRoomEffect, ExternalRoomEffect},
    DurableEffect, Effect, ExternalEffect, IngressPlan, PlanEffectDependency,
};
use jid::BareJid;
use waddle_xmpp::{
    inbox::{ConversationKind, InboxEntry},
    ingress::{ArchiveRole, IngressEffectIntent},
    mam::ArchivedMessage,
    pending_delivery::PendingPayload,
    Stanza,
};
use waddle_xmpp_core::xep0359::{StanzaId, NS_SID};
use xmpp_parsers::message::Message;

/// Re-stamp only identities belonging to this planned message. Historical
/// retraction, pin and reply targets remain unchanged, even under the same `by`.
/// The source plan is immutable so every serialization retry starts afresh.
pub fn restamp_plan(
    plan: &IngressPlan,
    recorded_archive_ids: &[(BareJid, ArchiveRole, StanzaId)],
) -> IngressPlan {
    let ids = Replacements::new(plan, recorded_archive_ids);
    let mut stamped = plan.clone();
    ids.message(&mut stamped.sanitized_message);
    if let Some(stanza) = &mut stamped.error_reply {
        ids.stanza(stanza);
    }
    for planned in &mut stamped.plan {
        for dependency in &mut planned.dependencies {
            if let PlanEffectDependency::AfterArchive { minted, .. } = dependency {
                ids.id(minted);
            }
        }
        match &mut planned.effect {
            Effect::Durable(effect) => ids.durable(effect),
            Effect::External(effect) => ids.external(effect),
            Effect::Immediate(_) => {}
        }
    }
    for intent in &mut stamped.intents {
        ids.intent(intent);
    }
    stamped
}

struct Replacements(Vec<(StanzaId, StanzaId)>);

impl Replacements {
    fn new(plan: &IngressPlan, recorded: &[(BareJid, ArchiveRole, StanzaId)]) -> Self {
        Self(
            plan.intents
                .iter()
                .filter_map(|intent| {
                    let (archive, role, stanza_id, by) = match intent {
                        IngressEffectIntent::ArchiveAuthoritative {
                            archive,
                            stanza_id,
                            by,
                            ..
                        } => (archive, ArchiveRole::Sender, stanza_id, by),
                        IngressEffectIntent::SystemMessageArchive {
                            archive,
                            sequence,
                            stanza_id,
                            by,
                            ..
                        } => (
                            archive,
                            ArchiveRole::SystemMessage {
                                sequence: *sequence,
                            },
                            stanza_id,
                            by,
                        ),
                        _ => return None,
                    };
                    recorded
                        .iter()
                        .find(|(recorded_archive, recorded_role, id)| {
                            recorded_archive == archive && *recorded_role == role && id.by == *by
                        })
                        .map(|(_, _, id)| (stanza_id.clone(), id.clone()))
                })
                .collect(),
        )
    }

    fn replacement(&self, id: &StanzaId) -> Option<&StanzaId> {
        self.0
            .iter()
            .find_map(|(minted, recorded)| (minted == id).then_some(recorded))
    }

    fn id(&self, id: &mut StanzaId) {
        if let Some(recorded) = self.replacement(id) {
            *id = recorded.clone();
        }
    }

    fn message(&self, message: &mut Message) {
        for payload in &mut message.payloads {
            self.element(payload);
        }
    }

    fn element(&self, element: &mut minidom::Element) {
        if element.is("stanza-id", NS_SID) {
            if let Some(mut id) = element.attr("id").and_then(|id| {
                Some(StanzaId::new(
                    id,
                    element.attr("by")?.parse::<jid::Jid>().ok()?,
                ))
            }) {
                self.id(&mut id);
                *element = waddle_xmpp_core::xep0359::build_stanza_id_element(&id.id, &id.by);
            }
        } else {
            for child in element.children_mut() {
                self.element(child);
            }
        }
    }

    fn stanza(&self, stanza: &mut Stanza) {
        if let Stanza::Message(message) = stanza {
            self.message(message);
        }
    }

    fn archive(&self, archive: &BareJid, message: &mut ArchivedMessage) {
        let mut id = StanzaId::new(&message.id, jid::Jid::from(archive.clone()));
        self.id(&mut id);
        message.id = id.id;
        if let Some(id) = &mut message.stanza_id {
            self.id(id);
        }
        // ArchivedMessage is the existing storage-boundary representation.
        if let Some(xml) = &mut message.stanza_xml {
            if let Ok(mut element) = xml.parse::<minidom::Element>() {
                self.element(&mut element);
                *xml = String::from(&element);
            }
        }
    }

    fn inbox(&self, owner: &BareJid, entry: &mut InboxEntry) {
        let archive = match entry.kind {
            ConversationKind::Direct => owner,
            ConversationKind::MucRoom => &entry.partner,
        };
        let mut id = StanzaId::new(&entry.last_stanza_id, jid::Jid::from(archive.clone()));
        self.id(&mut id);
        entry.last_stanza_id = id.id;
    }

    fn expectation(&self, expectation: &mut waddle_xmpp::mam::ArchiveExpectation) {
        if let waddle_xmpp::mam::ArchiveExpectation::Existing { stanza_id, .. } = expectation {
            self.id(stanza_id);
        }
    }

    fn durable(&self, effect: &mut DurableEffect) {
        match effect {
            DurableEffect::Direct(effect) => match effect {
                DurableDirectEffect::ArchiveDirect {
                    archive,
                    message,
                    archive_expectation,
                } => {
                    self.archive(archive, message);
                    self.expectation(archive_expectation);
                }
                DurableDirectEffect::ProjectInbox { owner, entry, .. } => self.inbox(owner, entry),
                DurableDirectEffect::DmCallThreadProjection { owner, mutation } => {
                    self.inbox_mutation(owner, mutation)
                }
                DurableDirectEffect::MarkInboxRead { .. }
                | DurableDirectEffect::RetractionTombstone { .. } => {}
            },
            DurableEffect::Room(effect) => match effect {
                DurableRoomEffect::ArchiveGroupchat {
                    room,
                    message,
                    archive_expectation,
                    ..
                } => {
                    self.archive(room, message);
                    self.expectation(archive_expectation);
                }
                DurableRoomEffect::ProjectGroupchatInbox {
                    owner,
                    entry,
                    recovery,
                    ..
                } => {
                    self.inbox(owner, entry);
                    if let Some(recovery) = recovery {
                        self.id(&mut recovery.key.archive_stanza_id);
                    }
                }
            },
        }
    }

    fn external(&self, effect: &mut ExternalEffect) {
        match effect {
            ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
                if let Some(waddle_xmpp::ingress::EffectMessageIdentity::StanzaId(id)) =
                    &mut route.route_identity
                {
                    self.id(id);
                }
                self.message(&mut route.message);
                match &mut route.fallback.payload {
                    PendingPayload::Archived(id) => self.id(id),
                    PendingPayload::Transient(message) => self.message(message),
                }
            }
            ExternalEffect::RoomMembershipMutation(_)
            | ExternalEffect::InviteLedger(_)
            | ExternalEffect::DmPinMutation(_) => {}
            ExternalEffect::Frame(stanza) => self.stanza(stanza),
            ExternalEffect::Direct(effect) => self.direct(effect),
            ExternalEffect::Room(effect) => self.room(effect),
            ExternalEffect::Delivery(effect) => self.delivery(effect),
        }
    }

    fn direct(&self, effect: &mut ExternalDirectEffect) {
        match effect {
            ExternalDirectEffect::NotificationActivity { mutation, .. } => {
                self.notification(mutation)
            }
            // The referenced durable projection owns the entry and is re-stamped above.
            ExternalDirectEffect::PushInboxUpdate { .. } => {}
            ExternalDirectEffect::LinkPreviewRefs { mutations }
            | ExternalDirectEffect::ClearLinkPreviewRefs { mutations } => {
                for mutation in mutations {
                    self.id(&mut mutation.current_archive_stanza_id);
                }
            }
            ExternalDirectEffect::DmCallThreadState { state } => {
                if let Some(id) = state
                    .active
                    .as_mut()
                    .and_then(|active| active.anchor.as_mut())
                {
                    self.id(id);
                }
            }
            ExternalDirectEffect::ScrubReplayForTombstone { .. } => {}
        }
    }

    fn candidate(&self, candidate: &mut crate::notification_outbox::NotificationCandidate) {
        if let Some(recorded) = self.replacement(candidate.archive_stanza_id()) {
            candidate.restamp_archive_id(recorded);
        }
    }

    fn room(&self, effect: &mut ExternalRoomEffect) {
        match effect {
            ExternalRoomEffect::ArchiveAfterPin {
                room,
                message,
                archive_expectation,
                ..
            } => {
                self.archive(room, message);
                self.expectation(archive_expectation);
            }
            ExternalRoomEffect::ObserveRoomMessage {
                message,
                error_request,
                ..
            } => {
                self.message(message);
                self.message(error_request);
            }
            ExternalRoomEffect::NotificationCandidate {
                archive_stanza_id,
                candidate,
                recovery,
                ..
            } => {
                self.id(archive_stanza_id);
                if let Some(candidate) = candidate {
                    self.candidate(candidate);
                }
                if let Some(recovery) = recovery {
                    self.id(&mut recovery.key.archive_stanza_id);
                }
            }
            #[cfg(feature = "clustering")]
            ExternalRoomEffect::RelayMucProxy { stanza, .. } => self.stanza(stanza),
            ExternalRoomEffect::RoomActorMutation { .. } => {}
        }
    }

    fn delivery(&self, effect: &mut ExternalDeliveryEffect) {
        match effect {
            ExternalDeliveryEffect::UndeliverableBounce { reply } => self.stanza(reply),
            ExternalDeliveryEffect::RouteToPeer { stanza, .. }
            | ExternalDeliveryEffect::QueueDetached { stanza, .. }
            | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
            | ExternalDeliveryEffect::RelayBareJid { stanza, .. } => self.stanza(stanza),
            ExternalDeliveryEffect::RelayCarbons { message, .. }
            | ExternalDeliveryEffect::Carbons { message, .. } => self.message(message),
            ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification,
                row,
                original_message,
            } => {
                match &mut row.payload {
                    PendingPayload::Archived(id) => self.id(id),
                    PendingPayload::Transient(message) => self.message(message),
                }
                self.message(original_message);
                if let PreparedOfflineNotification::Prepared(candidate) = prepared_notification {
                    self.candidate(candidate);
                }
            }
            ExternalDeliveryEffect::SfuRevokeToken { .. } => {}
        }
    }
}
