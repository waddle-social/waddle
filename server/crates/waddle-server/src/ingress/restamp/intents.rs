use super::Replacements;
use jid::BareJid;
use waddle_xmpp::ingress::{
    EffectMessageIdentity, InboxProjectionMutation, IngressEffectIntent,
    NotificationActivityMutation, PendingDeliveryMutation,
};

impl Replacements {
    pub(super) fn inbox_mutation(&self, owner: &BareJid, mutation: &mut InboxProjectionMutation) {
        match mutation {
            InboxProjectionMutation::Direct { entry, .. } => self.inbox(owner, entry),
            InboxProjectionMutation::DirectCallThreadAnchor {
                archive_stanza_id, ..
            } => self.id(archive_stanza_id),
            _ => {}
        }
    }

    pub(super) fn notification(&self, mutation: &mut NotificationActivityMutation) {
        match mutation {
            NotificationActivityMutation::OfflineDelivery {
                archive_stanza_id, ..
            }
            | NotificationActivityMutation::NotificationCandidate {
                archive_stanza_id, ..
            } => self.id(archive_stanza_id),
            _ => {}
        }
    }

    pub(super) fn intent(&self, intent: &mut IngressEffectIntent) {
        match intent {
            IngressEffectIntent::ArchiveAuthoritative { stanza_id, .. }
            | IngressEffectIntent::SystemMessageArchive { stanza_id, .. }
            | IngressEffectIntent::CallSignal { stanza_id, .. }
            | IngressEffectIntent::Extension { stanza_id, .. } => self.id(stanza_id),
            IngressEffectIntent::RouteDirect { route_identity, .. }
            | IngressEffectIntent::RouteMucGroupchat { route_identity, .. }
            | IngressEffectIntent::RouteMucSystemBroadcast { route_identity, .. } => {
                if let EffectMessageIdentity::StanzaId(id) = route_identity {
                    self.id(id);
                }
            }
            IngressEffectIntent::InboxProject { owner, mutation } => {
                self.inbox_mutation(owner, mutation)
            }
            IngressEffectIntent::NotificationActivityPreview { mutation, .. } => {
                self.notification(mutation)
            }
            IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                self.id(&mut mutation.archive_stanza_id)
            }
            IngressEffectIntent::PendingDelivery {
                mutation:
                    PendingDeliveryMutation::Archived {
                        archive_stanza_id, ..
                    },
            } => self.id(archive_stanza_id),
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => {
                self.id(&mut mutation.current_archive_stanza_id)
            }
            IngressEffectIntent::RetractionTombstone { mutation } => {
                self.id(&mut mutation.retraction_stanza_id)
            }
            // These intents carry historical targets or no archive identity.
            IngressEffectIntent::RouteOccupantPm { .. }
            | IngressEffectIntent::DispatchToRoomRemote { .. }
            | IngressEffectIntent::RecipientSmAppend { .. }
            | IngressEffectIntent::Carbons { .. }
            | IngressEffectIntent::RelayCarbons { .. }
            | IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Transient { .. },
            }
            | IngressEffectIntent::DmPinMutation { .. }
            | IngressEffectIntent::MucInviteMembershipGrant { .. }
            | IngressEffectIntent::MucInviteLedger { .. }
            | IngressEffectIntent::GroupDmMembershipGrant { .. }
            | IngressEffectIntent::GroupDmInviteLedger { .. }
            | IngressEffectIntent::RoomSubjectMutation { .. }
            | IngressEffectIntent::Pin { .. }
            | IngressEffectIntent::TombstoneReplayDeletion { .. }
            | IngressEffectIntent::ErrorReply { .. } => {}
        }
    }
}
