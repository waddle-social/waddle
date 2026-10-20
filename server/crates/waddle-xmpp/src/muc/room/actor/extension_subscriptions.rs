//! Room-owned subscription handles. Notification carries no message content: the
//! observer reads only committed, deduplicated work from durable storage.
use super::RoomActor;
use jid::BareJid;
use kameo::message::Context;
use std::sync::Arc;
use waddle_extensions::RoomObservationSubscription;

pub trait RoomExtensionSubscriber: Send + Sync {
    fn subscription(&self) -> &RoomObservationSubscription;
    /// Bounded, nonblocking wake hint. Overflow is repaired by durable recovery.
    fn try_notify(&self, room: &BareJid);
}

pub struct SubscribeRoomExtension {
    pub subscriber: Arc<dyn RoomExtensionSubscriber>,
}

impl kameo::message::Message<SubscribeRoomExtension> for RoomActor {
    type Reply = bool;
    async fn handle(
        &mut self,
        message: SubscribeRoomExtension,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let subscription = message.subscriber.subscription();
        if subscription.room != self.room.room_jid {
            return false;
        }
        if self
            .extension_subscribers
            .get(&subscription.plugin)
            .is_some_and(|current| {
                current.subscription().generation > subscription.generation
                    || (current.subscription().generation == subscription.generation
                        && current.subscription().identity != subscription.identity)
            })
        {
            return false;
        }
        self.extension_subscribers
            .insert(subscription.plugin.clone(), message.subscriber);
        true
    }
}

/// Sent only after the accepted message and observer work commit together.
pub struct NotifyRoomExtensions;
impl kameo::message::Message<NotifyRoomExtensions> for RoomActor {
    type Reply = ();
    async fn handle(
        &mut self,
        _message: NotifyRoomExtensions,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        for subscriber in self.extension_subscribers.values() {
            subscriber.try_notify(&self.room.room_jid);
        }
    }
}
