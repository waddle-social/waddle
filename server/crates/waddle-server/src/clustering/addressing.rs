//! Typed addressing: commands and deliveries to a room, an account or a
//! connection, without the caller choosing a route.
//!
//! A handler that has something for a room, an account or one bound
//! connection states the target and the operation. Whether the target's
//! owner is this node or another one, and which relay channel reaches it, is
//! decided behind this boundary by the ordered-relay delivery bridge. The
//! boundary adds no behaviour of its own: every operation is the bridge
//! operation of the same purpose, with the same route, the same wire message
//! and the same result type.
//!
//! Three rules hold for every operation here, and callers rely on them:
//!
//! - **The caller keeps the local path.** A result that says no relay was
//!   attempted means the local implementation is authoritative for this
//!   target, and the caller runs it.
//! - **A stale route is refreshed a bounded number of times.** When a send
//!   fails with an error that proves the resolved owner never saw it, the
//!   bridge re-resolves the owner once and repeats the operation under the
//!   same operation identity. If the claim has moved to this node, the
//!   bridge completes the operation through the local path itself and
//!   reports that as the operation's result. It does not refresh again.
//! - **No fallback after work that may have committed.** A result that
//!   reports an attempt, including an uncertain one, is final for this
//!   operation. The caller reports it; it must not run the local path or
//!   address another route.
//!
//! [`ClusteringHandles::addressing`] returns `None` on a node without the
//! relay substrate, where every target is local.

use std::sync::Arc;

use waddle_xmpp::Stanza;

use super::ordered_relay::{MucProxyOrigin, OrderedRelayMucProxyKind};
use super::route_bridge::{
    MucProxyRouteDecision, OrderedRelayDeliveryBridge, OrderedRelayMucProxyOutcome,
};
use super::ClusteringHandles;
use crate::ingress::identity::IngressRelayAdmission;
use crate::server::routes::interpret::OrderedRelayRouteOrigin;

impl ClusteringHandles {
    /// The addressing boundary of this node, when it has a relay substrate.
    pub(crate) fn addressing(&self) -> Option<Addressing<'_>> {
        self.ordered_relay_delivery_bridge
            .as_ref()
            .map(|bridge| Addressing { bridge })
    }
}

/// The node's addressing boundary. Borrowed from [`ClusteringHandles`] for
/// the duration of one handler.
#[derive(Clone, Copy)]
pub(crate) struct Addressing<'a> {
    bridge: &'a Arc<OrderedRelayDeliveryBridge>,
}

impl<'a> Addressing<'a> {
    /// Address a room by its bare JID.
    pub(crate) fn room(self, room: &'a jid::BareJid) -> RoomAddress<'a> {
        RoomAddress {
            bridge: self.bridge,
            room,
        }
    }
}

/// A room, addressed by its bare JID. The room's owner is whichever node
/// holds its `RoomActor` claim when the command is sent.
#[derive(Clone, Copy)]
pub(crate) struct RoomAddress<'a> {
    bridge: &'a Arc<OrderedRelayDeliveryBridge>,
    room: &'a jid::BareJid,
}

/// One stanza for a room, with the identities the room owner needs to
/// accept and order it.
pub(crate) struct RoomCommand<'c> {
    /// The stanza as the room should process it, already stamped with the
    /// authenticated sender.
    pub stanza: &'c Stanza,
    /// What the stanza is to the room, which selects the owner-side handler.
    pub kind: OrderedRelayMucProxyKind,
    /// Who the room should treat as the sender: a bound connection's
    /// occupancy session, or the server itself.
    pub sender: MucProxyOrigin,
    /// The ordering origin of the caller: the entity whose claim sequences
    /// this command relative to its other commands.
    pub origin: &'c OrderedRelayRouteOrigin,
    /// The ingress admission that accepted the stanza, when it was admitted
    /// through canonical ingress.
    pub admission: Option<&'c IngressRelayAdmission>,
}

impl RoomAddress<'_> {
    /// Send a command to the room's owner and report what was decided.
    ///
    /// [`MucProxyRouteDecision::Attempted`] carries the result of a relay to
    /// a remote owner and is final, whatever its outcome. Every other
    /// variant says why no relay was attempted; only
    /// [`MucProxyRouteDecision::LocalRoom`] and
    /// [`MucProxyRouteDecision::RoomUnclaimed`] establish that no remote
    /// owner exists.
    pub(crate) async fn command(&self, command: RoomCommand<'_>) -> MucProxyRouteDecision {
        self.bridge
            .try_proxy_muc_remote_decision(
                self.room,
                command.stanza,
                command.kind,
                command.sender,
                command.origin,
                command.admission,
            )
            .await
    }

    /// Send a command to the room's owner when that owner is another node.
    ///
    /// `Some` is the result of a relay attempt and is final. `None` means no
    /// relay was attempted and the caller keeps its local room path; it does
    /// not distinguish a locally owned room from one whose claim could not
    /// be used. Callers that must tell those apart use [`Self::command`].
    pub(crate) async fn command_if_remote(
        &self,
        command: RoomCommand<'_>,
    ) -> Option<OrderedRelayMucProxyOutcome> {
        self.bridge
            .try_proxy_muc_remote(
                self.room,
                command.stanza,
                command.kind,
                command.sender,
                command.origin,
                command.admission,
            )
            .await
    }
}
