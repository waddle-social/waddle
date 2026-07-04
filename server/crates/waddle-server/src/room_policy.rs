//! Adapter that exposes the live [`RoomRegistryActor`] to the T1 push
//! evaluator via the [`RoomPolicyStore`] trait.
//!
//! At T1 (outbox dispatch) the XEP-0492 evaluator needs to know
//! whether a groupchat conversation is members-only to project the
//! correct [`ConversationKind`] (PrivateGroup vs PublicGroup) and
//! pick the right default notification level. Slice 1 of #526 reads
//! this fresh from the live actor.
//!
//! Three result shapes are distinguished:
//!
//! - `Ok(Some(members_only))` — the room actor answered; the evaluator
//!   uses the typed bit.
//! - `Ok(None)` — the registry reports the room is not currently live.
//!   Expected/normal (restart windows, dormant rooms). The evaluator
//!   defers the candidate via policy-error backoff.
//! - `Err(NotificationOutboxError::RoomPolicyLookup { .. })` — an
//!   actor transport / mailbox failure. Surfaces actionable lookup
//!   failures to [`crate::notification_outbox::resolve_cached_room_policy`],
//!   which classifies the cache entry as
//!   `UnknownRoomPolicySource::LookupError` (private to the outbox gate
//!   module) and emits a single typed `warn!` per (drain batch, room). The
//!   evaluator then defers via the same policy-error backoff.
//!
//! Slice 2 will replace this with a durable T1 projection alongside
//! `notification_activity`, eliminating the deferral hole entirely.
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
            // Registry knows about no currently-live actor for this
            // room. Expected/normal — the evaluator defers.
            Ok(None) => return Ok(None),
            // Mailbox / transport failure. Surface as a typed lookup
            // error so the cache layer in `resolve_cached_room_policy`
            // classifies this as `UnknownRoomPolicySource::LookupError`
            // and emits its single per-batch `warn!`. Returning
            // `Ok(None)` here would otherwise mask actor failures as
            // routine dormancy.
            Err(error) => {
                return Err(NotificationOutboxError::RoomPolicyLookup {
                    room: room.clone(),
                    message: format!("RoomRegistryActor::GetRoom: {error}"),
                });
            }
        };
        match room_actor.ask(GetConfig).await {
            Ok(config) => Ok(Some(config.members_only)),
            // Same rationale as above: typed lookup error so the cache
            // layer fires its once-per-batch warn and classifies the
            // entry as `LookupError` rather than `NotLive`.
            Err(error) => Err(NotificationOutboxError::RoomPolicyLookup {
                room: room.clone(),
                message: format!("RoomActor::GetConfig: {error}"),
            }),
        }
    }
}
