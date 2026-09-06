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
    Delivery(super::super::routing::FullJidDeliveryOutcome),
    Unavailable,
}

impl super::PlannedEffect {
    pub(super) fn assumed_outcome(&self) -> EffectOutcome {
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
                entry,
                ..
            }))
            | Effect::Durable(DurableEffect::Room(DurableRoomEffect::ProjectGroupchatInbox {
                entry,
                ..
            })) => EffectOutcome::Inbox(Ok(entry.as_ref().clone())),
            Effect::External(super::ExternalEffect::Delivery(_)) => {
                EffectOutcome::Delivery(super::super::routing::FullJidDeliveryOutcome::Delivered)
            }
            _ => EffectOutcome::Completed,
        }
    }
}
