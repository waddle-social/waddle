//! Adapter that exposes the live [`RoomRegistryActor`] to the T1 push
//! evaluator via the [`RoomPolicyStore`] trait.
//!
//! At T1 (outbox dispatch) the XEP-0492 evaluator needs to know
//! whether a groupchat conversation is members-only to project the
//! correct [`ConversationKind`] (PrivateGroup vs PublicGroup) and
//! pick the right default notification level. Slice 1 of #526 reads
//! this fresh from the live actor; slice 2 will replace it with a
//! durable T1 projection alongside `notification_activity`.
//!
//! [`RoomRegistryActor`]: waddle_xmpp::muc::room_registry_actor::RoomRegistryActor
//! [`RoomPolicyStore`]: crate::notification_outbox::RoomPolicyStore
//! [`ConversationKind`]: crate::notification_settings_projection::ConversationKind

use async_trait::async_trait;
use jid::BareJid;
use kameo::actor::ActorRef;
use waddle_xmpp::muc::room_actor::GetConfig;
use waddle_xmpp::muc::room_registry_actor::{GetRoom, RoomRegistryActor};

use crate::notification_outbox::{NotificationOutboxError, RoomPolicyStore};

/// Live-actor adapter for [`RoomPolicyStore`].
#[derive(Clone)]
pub struct RoomRegistryActorPolicy {
    registry: ActorRef<RoomRegistryActor>,
}

impl RoomRegistryActorPolicy {
    pub fn new(registry: ActorRef<RoomRegistryActor>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl RoomPolicyStore for RoomRegistryActorPolicy {
    async fn room_members_only(
        &self,
        room: &BareJid,
    ) -> Result<Option<bool>, NotificationOutboxError> {
        let room_actor = match self
            .registry
            .ask(GetRoom {
                room_jid: room.clone(),
            })
            .await
        {
            Ok(Some(actor)) => actor,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::warn!(
                    room = %room,
                    %error,
                    "room registry GetRoom failed at T1 push gate; defaulting to public"
                );
                return Ok(None);
            }
        };
        match room_actor.ask(GetConfig).await {
            Ok(config) => Ok(Some(config.members_only)),
            Err(error) => {
                tracing::warn!(
                    room = %room,
                    %error,
                    "room actor GetConfig failed at T1 push gate; defaulting to public"
                );
                Ok(None)
            }
        }
    }
}
