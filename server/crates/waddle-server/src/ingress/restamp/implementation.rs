//! Implementation behind the parent module facade.

use super::ArchivedMessage;
use super::BareJid;
use super::IngressUowError;
use super::Message;
use super::NotificationCandidate;
use super::PendingPayload;
use super::Stanza;
use super::{ArchiveRole, IngressEffectIntent};
use super::{ConversationKind, InboxEntry};
use super::{DurableDirectEffect, ExternalDirectEffect};
use super::{DurableEffect, Effect, ExternalEffect, IngressPlan, PlanEffectDependency};
use super::{DurableRoomEffect, ExternalRoomEffect};
use super::{ExternalDeliveryEffect, PreparedOfflineNotification};
use super::{StanzaId, NS_SID};

/// Re-stamp only identities belonging to this planned message. Historical
/// retraction, pin and reply targets remain unchanged, even under the same `by`.
/// The source plan is immutable so every serialization retry starts afresh.
pub fn restamp_plan(
    plan: &IngressPlan,
    recorded_archive_ids: &[(BareJid, ArchiveRole, StanzaId)],
) -> Result<IngressPlan, IngressUowError> {
    let ids = Replacements::new(plan, recorded_archive_ids);
    let mut stamped = plan.clone();
    ids.message(&mut stamped.sanitized_message);
    if let Some(message) = &mut stamped.room_canonical_message {
        ids.message(message);
    }
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
            Effect::External(effect) => ids.external(effect)?,
            Effect::Immediate(_) => {}
        }
    }
    for intent in &mut stamped.intents {
        ids.intent(intent)?;
    }
    Ok(stamped)
}

/// Restore route-derived stamps, including prepared copies with no MAM row.
/// Only exact provisional IDs from a matching recorded authority are replaced;
/// payload and audience changes remain visible to reconciliation.
pub(in crate::ingress) fn restore_route_identities(
    plan: &mut IngressPlan,
    recorded: &[IngressEffectIntent],
) -> Result<(), IngressUowError> {
    use waddle_xmpp::ingress::EffectMessageIdentity;
    let mut replacements = Vec::new();
    for planned in &plan.intents {
        replacements.extend(prepared_route_ids(planned, recorded));
        let IngressEffectIntent::RouteMucGroupchat {
            route_identity: EffectMessageIdentity::StanzaId(minted),
            ..
        } = planned
        else {
            continue;
        };
        let mut candidates = recorded.iter().filter(|saved| {
            matches!(saved, IngressEffectIntent::RouteMucGroupchat { .. })
                && saved.authority_key() == planned.authority_key()
        });
        let Some(IngressEffectIntent::RouteMucGroupchat {
            route_identity: EffectMessageIdentity::StanzaId(frozen),
            ..
        }) = candidates.next()
        else {
            continue;
        };
        if candidates.next().is_none() && minted.by == frozen.by {
            replacements.push((minted.clone(), frozen.clone()));
        }
    }
    let ids = Replacements(replacements);
    ids.message(&mut plan.sanitized_message);
    if let Some(message) = &mut plan.room_canonical_message {
        ids.message(message);
    }
    for planned in &mut plan.plan {
        for dependency in &mut planned.dependencies {
            if let PlanEffectDependency::AfterArchive { minted, .. } = dependency {
                ids.id(minted);
            }
        }
        match &mut planned.effect {
            Effect::Durable(effect) => ids.durable(effect),
            Effect::External(effect) => ids.external(effect)?,
            Effect::Immediate(_) => {}
        }
    }
    for intent in &mut plan.intents {
        ids.intent(intent)?;
    }
    Ok(())
}

pub(in crate::ingress::restamp) fn prepared_route_ids(
    planned: &IngressEffectIntent,
    recorded: &[IngressEffectIntent],
) -> Vec<(StanzaId, StanzaId)> {
    use waddle_xmpp_core::xep0359::extract_stanza_ids;
    let IngressEffectIntent::RouteDirect {
        recipient,
        prepared: Some(offered),
        ..
    } = planned
    else {
        return Vec::new();
    };
    let mut candidates = recorded
        .iter()
        .filter(|saved| saved.semantic_key() == planned.semantic_key());
    let Some(IngressEffectIntent::RouteDirect {
        prepared: Some(saved),
        ..
    }) = candidates.next()
    else {
        return Vec::new();
    };
    let offered = offered.message();
    let saved = saved.message();
    let Some(sender) = offered.from.as_ref().map(jid::Jid::to_bare) else {
        return Vec::new();
    };
    if candidates.next().is_some()
        || saved.from.as_ref().map(jid::Jid::to_bare).as_ref() != Some(&sender)
        || offered.to != saved.to
        || offered.type_ != saved.type_
    {
        return Vec::new();
    }
    let mut owners = vec![sender];
    if !owners.contains(recipient) {
        owners.push(recipient.clone());
    }
    let minted = extract_stanza_ids(offered);
    let frozen = extract_stanza_ids(saved);
    owners
        .into_iter()
        .filter_map(|owner| {
            let owner: jid::Jid = owner.into();
            let mut provisional = minted.iter().filter(|id| id.by == owner);
            let mut original = frozen.iter().filter(|id| id.by == owner);
            let pair = (provisional.next()?.clone(), original.next()?.clone());
            (provisional.next().is_none() && original.next().is_none()).then_some(pair)
        })
        .collect()
}

pub(in crate::ingress::restamp) struct Replacements(
    pub(in crate::ingress::restamp) Vec<(StanzaId, StanzaId)>,
);

impl Replacements {
    pub(in crate::ingress::restamp) fn new(
        plan: &IngressPlan,
        recorded: &[(BareJid, ArchiveRole, StanzaId)],
    ) -> Self {
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

    pub(in crate::ingress::restamp) fn replacement(&self, id: &StanzaId) -> Option<&StanzaId> {
        self.0
            .iter()
            .find_map(|(minted, recorded)| (minted == id).then_some(recorded))
    }

    pub(in crate::ingress::restamp) fn id(&self, id: &mut StanzaId) {
        if let Some(recorded) = self.replacement(id) {
            *id = recorded.clone();
        }
    }

    pub(in crate::ingress::restamp) fn message(&self, message: &mut Message) {
        for payload in &mut message.payloads {
            self.element(payload);
        }
    }

    pub(in crate::ingress::restamp) fn element(&self, element: &mut minidom::Element) {
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

    pub(in crate::ingress::restamp) fn stanza(&self, stanza: &mut Stanza) {
        if let Stanza::Message(message) = stanza {
            self.message(message);
        }
    }

    pub(in crate::ingress::restamp) fn archive(
        &self,
        archive: &BareJid,
        message: &mut ArchivedMessage,
    ) {
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

    pub(in crate::ingress::restamp) fn inbox(&self, owner: &BareJid, entry: &mut InboxEntry) {
        let archive = match entry.kind {
            ConversationKind::Direct => owner,
            ConversationKind::MucRoom => &entry.partner,
        };
        let mut id = StanzaId::new(&entry.last_stanza_id, jid::Jid::from(archive.clone()));
        self.id(&mut id);
        entry.last_stanza_id = id.id;
    }

    pub(in crate::ingress::restamp) fn expectation(
        &self,
        expectation: &mut waddle_xmpp::mam::ArchiveExpectation,
    ) {
        if let waddle_xmpp::mam::ArchiveExpectation::Existing { stanza_id, .. } = expectation {
            self.id(stanza_id);
        }
    }

    pub(in crate::ingress::restamp) fn durable(&self, effect: &mut DurableEffect) {
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
                    archive_stanza_id,
                    recovery,
                    ..
                } => {
                    self.id(archive_stanza_id);
                    if let Some(recovery) = recovery {
                        self.id(&mut recovery.key.archive_stanza_id);
                    }
                }
            },
        }
    }

    pub(in crate::ingress::restamp) fn external(
        &self,
        effect: &mut ExternalEffect,
    ) -> Result<(), IngressUowError> {
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
            ExternalEffect::Direct(effect) => self.direct(effect)?,
            ExternalEffect::Room(effect) => self.room(effect),
            ExternalEffect::Delivery(effect) => self.delivery(effect),
        }
        Ok(())
    }

    pub(in crate::ingress::restamp) fn direct(
        &self,
        effect: &mut ExternalDirectEffect,
    ) -> Result<(), IngressUowError> {
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
            ExternalDirectEffect::DmCallThreadState { state, receipt } => {
                if let Some(receipt) = receipt {
                    self.intent(receipt)?;
                }
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
        Ok(())
    }

    pub(in crate::ingress::restamp) fn candidate(&self, candidate: &mut NotificationCandidate) {
        if let Some(recorded) = self.replacement(candidate.archive_stanza_id()) {
            candidate.restamp_archive_id(recorded);
        }
    }

    pub(in crate::ingress::restamp) fn room(&self, effect: &mut ExternalRoomEffect) {
        match effect {
            ExternalRoomEffect::ArchiveAfterPin { room, message, .. } => {
                self.archive(room, message);
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
            ExternalRoomEffect::RoomActorMutation {
                mutation:
                    crate::server::routes::interpret::effects::room::RoomActorMutation::SetSubject {
                        rejection_reply,
                        ..
                    },
                ..
            } => self.message(rejection_reply),
            ExternalRoomEffect::RoomActorMutation { .. } => {}
        }
    }

    pub(in crate::ingress::restamp) fn delivery(&self, effect: &mut ExternalDeliveryEffect) {
        match effect {
            ExternalDeliveryEffect::HostOwnedCopy { stanza, .. } => self.stanza(stanza),
            ExternalDeliveryEffect::UndeliverableBounce { reply } => self.stanza(reply),
            ExternalDeliveryEffect::RouteToPeer {
                stanza,
                route_identity,
                ..
            }
            | ExternalDeliveryEffect::QueueDetached {
                stanza,
                route_identity,
                ..
            }
            | ExternalDeliveryEffect::RelayFullJid {
                stanza,
                route_identity,
                ..
            } => {
                self.stanza(stanza);
                if let Some(waddle_xmpp::ingress::EffectMessageIdentity::StanzaId(id)) =
                    route_identity
                {
                    self.id(id);
                }
            }
            ExternalDeliveryEffect::RelayBareJid { stanza, .. } => self.stanza(stanza),
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
