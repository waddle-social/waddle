//! Room-specific routing decisions shared by planning and immediate dispatch.
//!
//! Planning only reads the claim. Immediate dispatch delegates to the existing
//! bridge, whose attempted outcome may already have committed remote work.

use super::effects::PlanFailure;
use super::{BareJid, Deps, Message, Stanza};
use crate::clustering::addressing::{Ownership, OwnershipLookupFailed};
use crate::clustering::ordered_relay::{MucProxyOrigin, OrderedRelayMucProxyKind};
use crate::clustering::route_bridge::MucProxyRouteDecision;
use waddle_xmpp::ingress::RelayTargetIdentity;

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
        match clustering.room_owner(self.room).await {
            Ok(Ownership::NotClustered | Ownership::Unclaimed | Ownership::Local) => {
                PlannedRoomRoute::Local
            }
            Ok(Ownership::Remote(owner)) => PlannedRoomRoute::Remote(
                RelayTargetIdentity::owner_node(owner.node_id, owner.node_epoch),
            ),
            Ok(Ownership::Stale) => PlannedRoomRoute::Unavailable(PlanFailure::RoomClaimStale),
            Err(OwnershipLookupFailed) => {
                PlannedRoomRoute::Unavailable(PlanFailure::OwnershipLookup)
            }
        }
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
        let Some(addressing) = state.deps.app_state.clustering_claims.addressing() else {
            return ImmediateRoomRoute::Local;
        };
        ImmediateRoomRoute::Decision(
            addressing
                .room(self.room)
                .command(crate::clustering::addressing::RoomCommand {
                    stanza: &Stanza::Message(self.message.clone()),
                    kind: OrderedRelayMucProxyKind::GroupchatMessage,
                    sender: MucProxyOrigin::Server,
                    origin,
                    admission: None,
                })
                .await,
        )
    }
}
