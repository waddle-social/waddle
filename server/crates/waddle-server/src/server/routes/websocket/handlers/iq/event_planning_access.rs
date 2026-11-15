//! Room-incarnation scoped authorization for private event workspaces.
//!
//! A single owner-actor snapshot binds affiliation and the immutable room
//! lifecycle. A stale request can only write its former lifecycle's backing
//! node; a room recreated at the same JID cannot read that data.
use std::time::Duration;

#[cfg(feature = "clustering")]
use jid::BareJid;
use jid::FullJid;
use kameo::actor::ActorRef;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp::muc::room_actor::{GetSnapshot, RoomActor};
use waddle_xmpp::Affiliation;
use waddle_xmpp_core::event_planning::NODE_EVENT_PLANNING;

#[cfg(feature = "clustering")]
use super::{get_room_actor_result, WebSocketState};
#[cfg(feature = "clustering")]
use crate::clustering::relay::{RelayEventPlanningAccess, RelayEventPlanningAccessReply};
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::extension_room_owner;

const OWNER_BUDGET: Duration = Duration::from_secs(4);
#[cfg(feature = "clustering")]
const ORIGIN_BUDGET: Duration = Duration::from_secs(11);

/// Immutable room incarnation, independent of membership revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPlanningGeneration(pub(crate) Uuid);

/// One atomic owner snapshot used for both access and backing-node selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPlanningAccess {
    pub affiliation: Affiliation,
    pub(crate) generation: EventPlanningGeneration,
}

impl EventPlanningAccess {
    pub(super) fn storage_node(self) -> String {
        format!("{NODE_EVENT_PLANNING}:{}", self.generation.0)
    }
}

/// Snapshot affiliation and room incarnation in a single room-actor turn.
pub(super) async fn local_event_planning_access(
    actor: &ActorRef<RoomActor>,
    requester: &FullJid,
) -> Result<EventPlanningAccess, ()> {
    let snapshot = actor
        .ask(GetSnapshot)
        .reply_timeout(OWNER_BUDGET)
        .await
        .map_err(|_| ())?;
    let generation = snapshot
        .durable_coordinates
        .map(|coordinates| coordinates.lifecycle.as_uuid())
        .unwrap_or_else(|| snapshot.occupancy_incarnation.as_uuid());
    Ok(EventPlanningAccess {
        affiliation: snapshot.room.affiliation(&requester.to_bare()),
        generation: EventPlanningGeneration(generation),
    })
}

/// Test seam replacing only the transport hop, like the muc#admin relay.
#[cfg(all(test, feature = "clustering"))]
pub(crate) type TestEventPlanningOwnerRelay = std::sync::Arc<
    dyn Fn(
            RelayEventPlanningAccess,
        )
            -> futures::future::BoxFuture<'static, Result<RelayEventPlanningAccessReply, ()>>
        + Send
        + Sync,
>;

#[cfg(all(test, feature = "clustering"))]
tokio::task_local! {
    pub(crate) static TEST_EVENT_PLANNING_OWNER_RELAY: TestEventPlanningOwnerRelay;
}

#[cfg(feature = "clustering")]
async fn ask_owner(
    state: &WebSocketState,
    owner: &waddle_xmpp::ownership::NodeIdentity,
    message: RelayEventPlanningAccess,
) -> Result<RelayEventPlanningAccessReply, ()> {
    #[cfg(test)]
    if let Ok(relay) = TEST_EVENT_PLANNING_OWNER_RELAY.try_with(std::sync::Arc::clone) {
        return relay(message).await;
    }
    let bridge = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
        .ok_or(())?;
    let mut handle = bridge.event_planning_access_handle(owner);
    handle.event_planning_access(message).await.map_err(|_| ())
}

/// Obtain the authoritative affiliation when no local room actor exists.
/// `None` means no room owner, while `Err` is an unavailable owner and must
/// produce a retryable error rather than an authorization denial.
#[cfg(feature = "clustering")]
pub(super) async fn remote_event_planning_access(
    state: &WebSocketState,
    room: &BareJid,
    requester: &FullJid,
) -> Result<Option<EventPlanningAccess>, ()> {
    tokio::time::timeout(ORIGIN_BUDGET, async {
        let message = RelayEventPlanningAccess {
            room: room.clone(),
            requester: requester.clone(),
            trace: Default::default(),
        };
        for _ in 0..2 {
            let Some(owner) = extension_room_owner(state, room).await.map_err(|_| ())? else {
                return Ok(None);
            };
            match ask_owner(state, &owner, message.clone()).await {
                Ok(RelayEventPlanningAccessReply::Access(access)) => {
                    return Ok(Some(access));
                }
                // Ownership can move between the claim read and actor ask.
                Ok(RelayEventPlanningAccessReply::NotOwner) => continue,
                Ok(RelayEventPlanningAccessReply::Unavailable) | Err(_) => return Err(()),
            }
        }
        Err(())
    })
    .await
    .map_err(|_| ())?
}

/// Owner-side handler. It never forwards and cannot mutate the room.
#[cfg(feature = "clustering")]
pub(crate) async fn relayed_event_planning_access(
    state: &WebSocketState,
    message: RelayEventPlanningAccess,
) -> RelayEventPlanningAccessReply {
    match tokio::time::timeout(OWNER_BUDGET, async {
        // A live local actor alone is not proof: a claim may have moved
        // before that predecessor has been stopped.
        match extension_room_owner(state, &message.room).await {
            Ok(Some(_)) => return RelayEventPlanningAccessReply::NotOwner,
            Err(_) => return RelayEventPlanningAccessReply::Unavailable,
            Ok(None) => {}
        }
        let actor = match get_room_actor_result(state, &message.room).await {
            Ok(Some(actor)) => actor,
            Ok(None) => return RelayEventPlanningAccessReply::NotOwner,
            Err(_) => return RelayEventPlanningAccessReply::Unavailable,
        };
        match local_event_planning_access(&actor, &message.requester).await {
            Ok(access) => RelayEventPlanningAccessReply::Access(access),
            Err(_) => RelayEventPlanningAccessReply::Unavailable,
        }
    })
    .await
    {
        Ok(reply) => reply,
        Err(_) => RelayEventPlanningAccessReply::Unavailable,
    }
}

#[cfg(all(test, feature = "clustering"))]
#[path = "event_planning_access/tests.rs"]
mod tests;
