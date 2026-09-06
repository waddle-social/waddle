//! Typed results shared by assumed planning and immediate execution.
use waddle_xmpp::{
    inbox::{storage::InboxStorageError, InboxEntry},
    mam::{storage::MamStorageError, StoreOutcome},
    Stanza,
};

#[derive(Debug)]
pub enum EffectOutcome {
    Completed,
    Frames(Vec<Stanza>),
    Archive(Result<StoreOutcome, MamStorageError>),
    Inbox(Result<InboxEntry, InboxStorageError>),
    PlannedInbox(ProjectionRef),
    Delivery(super::super::routing::FullJidDeliveryOutcome),
    Unavailable,
}

impl super::PlannedEffect {
    pub(super) fn assumed_outcome(&self, projection: ProjectionRef) -> EffectOutcome {
        use super::{direct::DurableDirectEffect, room::DurableRoomEffect, DurableEffect, Effect};
        match &self.effect {
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ArchiveDirect {
                message,
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ArchiveGroupchat {
                message,
                ..
            })) => EffectOutcome::Archive(Ok(StoreOutcome::Stored(message.id.clone()))),
            Effect::Durable(DurableEffect::Direct(DurableDirectEffect::ProjectInbox {
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                ..
            }))
            | Effect::Durable(DurableEffect::Direct(DurableDirectEffect::MarkInboxRead {
                ..
            })) => EffectOutcome::PlannedInbox(projection),
            Effect::External(super::ExternalEffect::Delivery(_)) => {
                EffectOutcome::Delivery(super::super::routing::FullJidDeliveryOutcome::Delivered)
            }
            _ => EffectOutcome::Completed,
        }
    }
}

/// Index of an inbox-producing durable operation in the unchanged ingress plan.
/// It remains stable when archive identities are reconciled before commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProjectionRef(pub usize);

/// Successful values returned by Phase B, published only after commit.
#[derive(Clone, Debug)]
pub enum DurableOutcome {
    Inbox(InboxEntry),
}

/// Outcomes belong to one plan and one committed transaction attempt.
#[derive(Debug, Default)]
pub struct AppliedDurableEffects {
    outcomes: std::collections::HashMap<ProjectionRef, DurableOutcome>,
}

impl AppliedDurableEffects {
    pub fn insert(&mut self, projection: ProjectionRef, outcome: DurableOutcome) {
        self.outcomes.insert(projection, outcome);
    }

    pub fn inbox(&self, projection: ProjectionRef) -> Option<&InboxEntry> {
        match self.outcomes.get(&projection)? {
            DurableOutcome::Inbox(entry) => Some(entry),
        }
    }
}
