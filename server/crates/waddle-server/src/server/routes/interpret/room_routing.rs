//! Room-specific routing decisions shared by planning and immediate dispatch.
//!
//! Planning only reads the claim. Immediate dispatch delegates to the existing
//! bridge, whose attempted outcome may already have committed remote work.

use super::effects::PlanFailure;
use super::{BareJid, Deps, Message, Stanza};
use crate::clustering::ordered_relay::{MucProxyOrigin, OrderedRelayMucProxyKind};
use crate::clustering::route_bridge::MucProxyRouteDecision;
use waddle_xmpp::ingress::RelayTargetIdentity;
use waddle_xmpp::ownership::{Entity, EntityType};

/// One typed room command. The route is resolved separately for a read-only
/// plan and for an immediate attempt; neither may retarget a frozen effect.
pub(super) struct RoomMessageRoute<'a> {
    pub room: &'a BareJid,
    pub message: &'a Message,
}

pub(super) enum PlannedRoomRoute {
    Local,
    Remote(RelayTargetIdentity),
    Unavailable(PlanFailure),
}

pub(super) enum ImmediateRoomRoute {
    Local,
    Decision(MucProxyRouteDecision),
}

impl RoomMessageRoute<'_> {
    /// Resolve authority without sending or recording effects. The caller
    /// freezes a remote target in its existing plan and intent.
    pub(super) async fn plan(&self, deps: &Deps<'_>) -> PlannedRoomRoute {
        let Some(state) = deps.web_socket_state else {
            return PlannedRoomRoute::Local;
        };
        let clustering = &state.deps.app_state.clustering_claims;
        if deps.ordered_relay_origin.is_none() {
            return PlannedRoomRoute::Local;
        }
        let Some(store) = clustering.claim_store.as_ref() else {
            return PlannedRoomRoute::Local;
        };
        let entity = Entity::new(EntityType::RoomActor, self.room.to_string());
        let claim = match store.current_claim(&entity).await {
            Ok(None) => return PlannedRoomRoute::Local,
            Ok(Some(claim)) if claim.owner_lease_fresh => claim,
            Ok(Some(_)) => return PlannedRoomRoute::Unavailable(PlanFailure::RoomClaimStale),
            Err(_) => return PlannedRoomRoute::Unavailable(PlanFailure::OwnershipLookup),
        };
        if clustering
            .node_identity
            .as_ref()
            .is_some_and(|identity| identity.current() == claim.owner)
        {
            return PlannedRoomRoute::Local;
        }
        PlannedRoomRoute::Remote(RelayTargetIdentity::owner_node(
            claim.owner.node_id,
            claim.owner.node_epoch,
        ))
    }

    /// Perform the existing bridge operation once. The bridge retains its
    /// bounded owner refresh and exact attempt result; callers must not
    /// interpret an attempted failure as permission for local fallback.
    pub(super) async fn execute(&self, deps: &Deps<'_>) -> ImmediateRoomRoute {
        let Some(state) = deps.web_socket_state else {
            return ImmediateRoomRoute::Local;
        };
        let Some(origin) = deps.ordered_relay_origin.as_ref() else {
            return ImmediateRoomRoute::Local;
        };
        let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return ImmediateRoomRoute::Local;
        };
        ImmediateRoomRoute::Decision(
            bridge
                .try_proxy_muc_remote_decision(
                    self.room,
                    &Stanza::Message(self.message.clone()),
                    OrderedRelayMucProxyKind::GroupchatMessage,
                    MucProxyOrigin::Server,
                    origin,
                    None,
                )
                .await,
        )
    }
}
