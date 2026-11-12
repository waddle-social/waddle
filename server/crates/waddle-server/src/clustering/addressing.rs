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

use std::future::Future;
use std::sync::Arc;

use waddle_xmpp::protocol::CarbonKind;
use waddle_xmpp::registry::DeliveryKind;
use waddle_xmpp::roster::{RosterItem, RosterVersion};
use waddle_xmpp::Stanza;

use super::ordered_relay::{MucProxyOrigin, OrderedRelayMucProxyKind};
use super::route_bridge::{
    MucProxyRouteDecision, OrderedRelayDeliveryBridge, OrderedRelayMucProxyOutcome,
    RegisteredRemoteWriteAcceptedDelivery, RemoteCarbonFanout,
};
use super::ClusteringHandles;
use crate::ingress::identity::{IngressAppendObligationRef, IngressRelayAdmission};
use crate::server::routes::interpret::{
    FullJidDeliveryOutcome, OrderedRelayRouteOrigin, RecipientInventory, SmIngressAppendContext,
};
use waddle_xmpp::ownership::{Entity, EntityType, NodeIdentity};

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
    /// Address an account by its bare JID.
    pub(crate) fn account(self, account: &'a jid::BareJid) -> AccountAddress<'a> {
        AccountAddress {
            bridge: self.bridge,
            account,
        }
    }

    /// Address one bound connection by its full JID.
    pub(crate) fn connection(self, connection: &'a jid::FullJid) -> ConnectionAddress<'a> {
        ConnectionAddress {
            bridge: self.bridge,
            connection,
        }
    }

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

/// An account, addressed by its bare JID. The account's owner is whichever
/// node holds its `UserActor` claim when the operation is sent.
#[derive(Clone, Copy)]
pub(crate) struct AccountAddress<'a> {
    bridge: &'a Arc<OrderedRelayDeliveryBridge>,
    account: &'a jid::BareJid,
}

impl<'a> AccountAddress<'a> {
    /// Deliver a stanza addressed to the bare JID to the account's owner,
    /// which applies its own resource selection.
    ///
    /// `Some` is the result of a relay attempt and is final. `None` means no
    /// relay was attempted and the caller keeps its local bare-JID path.
    pub(crate) fn deliver(
        self,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
    ) -> impl Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a {
        self.bridge
            .try_deliver_bare_jid_remote(self.account, stanza, origin)
    }

    /// Ask the account's owner to fan a carbon copy out to the account's
    /// other resources. `source` is the resource the copied message was sent
    /// from or delivered to; `exclude` lists resources that must not receive
    /// a copy.
    ///
    /// `None` means the owner is this node or `source` is not registered
    /// with a remote owner, and the caller runs the local fan-out.
    pub(crate) async fn fan_out_carbons(
        &self,
        source: &jid::FullJid,
        message: &xmpp_parsers::message::Message,
        kind: CarbonKind,
        exclude: Vec<jid::FullJid>,
        ingress_append: Option<&IngressAppendObligationRef>,
    ) -> Option<RemoteCarbonFanout> {
        self.bridge
            .try_fanout_remote_user_carbons(
                source,
                self.account,
                message,
                kind,
                exclude,
                ingress_append,
            )
            .await
    }

    /// Ask the account's owner to push a roster change to the account's
    /// interested resources. Returns whether a remote owner took the push;
    /// `false` leaves the push to the caller's local path.
    pub(crate) async fn push_roster(
        &self,
        source: &jid::FullJid,
        item: &RosterItem,
        version: &RosterVersion,
    ) -> bool {
        self.bridge
            .try_fanout_remote_user_roster_push(source, self.account, item, version)
            .await
    }

    /// Ask the account's owner to push a block-list change to the account's
    /// interested resources. Returns whether a remote owner took the push;
    /// `false` leaves the push to the caller's local path.
    pub(crate) async fn push_blocklist(
        &self,
        source: &jid::FullJid,
        blocked: bool,
        jids: &[jid::Jid],
    ) -> bool {
        self.bridge
            .try_fanout_remote_user_blocklist_push(source, self.account, blocked, jids)
            .await
    }

    /// The account owner's view of the account's deliverable resources.
    ///
    /// `Ok(None)` means the owner is this node and the caller reads its own
    /// registry. `Err` means a remote owner exists but did not answer; the
    /// caller must not substitute its local view.
    pub(crate) async fn recipient_inventory(&self) -> Result<Option<RecipientInventory>, ()> {
        self.bridge.recipient_inventory_remote(self.account).await
    }
}

/// One bound connection, addressed by its full JID.
///
/// Two different nodes can matter for a connection: the owner of the
/// account's `UserActor` claim, which orders deliveries to the account's
/// resources, and the node that hosts the socket. The `deliver*` operations
/// address the account owner when it is another node. The
/// `deliver_to_registered_remote*` operations are for the account owner
/// itself: they reach a socket that another node hosts and has registered
/// here.
#[derive(Clone, Copy)]
pub(crate) struct ConnectionAddress<'a> {
    bridge: &'a Arc<OrderedRelayDeliveryBridge>,
    connection: &'a jid::FullJid,
}

impl<'a> ConnectionAddress<'a> {
    /// Deliver a stanza to this exact resource through the account's owner.
    ///
    /// `Some` is the result of a relay attempt and is final. `None` means no
    /// relay was attempted and the caller keeps its local full-JID path.
    pub(crate) fn deliver(
        self,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
        call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
        ingress_append_context: Option<SmIngressAppendContext>,
    ) -> impl Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a {
        self.bridge.try_deliver_full_jid_remote(
            self.connection,
            stanza,
            origin,
            call_setup,
            ingress_append_context,
        )
    }

    /// [`Self::deliver`] for a stanza whose receive time was fixed earlier,
    /// so the owner stamps and archives it with that time.
    pub(crate) fn deliver_at(
        self,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
        call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
        ingress_append_context: Option<SmIngressAppendContext>,
        received_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> impl Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a {
        self.bridge.try_deliver_full_jid_remote_at(
            self.connection,
            stanza,
            origin,
            call_setup,
            ingress_append_context,
            received_at,
        )
    }

    /// [`Self::deliver`] for a stanza this node has already processed
    /// (archived, carbon-copied, policy-checked): the owner only writes it
    /// to the resource.
    pub(crate) fn deliver_processed(
        self,
        stanza: &'a Stanza,
        origin: &'a OrderedRelayRouteOrigin,
        ingress_append_context: Option<SmIngressAppendContext>,
    ) -> impl Future<Output = Option<FullJidDeliveryOutcome>> + Send + 'a {
        self.bridge.try_deliver_processed_full_jid_remote(
            self.connection,
            stanza,
            origin,
            ingress_append_context,
        )
    }

    /// Deliver a frame to this resource when its socket is hosted by another
    /// node and registered with this account owner.
    ///
    /// `Some` is the result of a relay attempt and is final. `None` means
    /// the resource is not a registered remote one, and the caller keeps its
    /// local delivery path.
    pub(crate) async fn deliver_to_registered_remote(
        &self,
        stanza: &Stanza,
        kind: DeliveryKind,
        ingress_append_context: Option<&SmIngressAppendContext>,
    ) -> Option<FullJidDeliveryOutcome> {
        self.bridge
            .try_deliver_registered_remote_resource(
                self.connection,
                stanza,
                kind,
                ingress_append_context,
            )
            .await
    }

    /// [`Self::deliver_to_registered_remote`] for a stanza whose receive
    /// time was fixed earlier.
    pub(crate) async fn deliver_to_registered_remote_at(
        &self,
        stanza: &Stanza,
        kind: DeliveryKind,
        ingress_append_context: Option<&SmIngressAppendContext>,
        received_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Option<FullJidDeliveryOutcome> {
        self.bridge
            .try_deliver_registered_remote_resource_at(
                self.connection,
                stanza,
                kind,
                ingress_append_context,
                received_at,
            )
            .await
    }

    /// Deliver a frame to a registered remote resource and wait until the
    /// hosting node reports that its socket writer accepted it.
    pub(crate) async fn deliver_to_registered_remote_write_accepted(
        &self,
        stanza: &Stanza,
    ) -> RegisteredRemoteWriteAcceptedDelivery {
        self.bridge
            .try_deliver_registered_remote_resource_write_accepted(self.connection, stanza)
            .await
    }
}

/// Who owns an addressed room or account right now.
///
/// This is a read of the claim, taken without sending anything, for callers
/// that plan work before they may send it. It is advisory: the claim can
/// move after the read, and the operation that later sends re-resolves the
/// owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ownership {
    /// This node has no claim store: it is not clustered, and every target
    /// is local.
    NotClustered,
    /// No claim exists for the target. No node hosts it.
    Unclaimed,
    /// This node holds the claim with a fresh lease.
    Local,
    /// Another node holds the claim with a fresh lease. A node without a
    /// node identity of its own reports every fresh claim as remote.
    Remote(NodeIdentity),
    /// A claim exists but its owner's lease is stale: the owner may have
    /// crashed or be late renewing. The target cannot be used until the
    /// claim is renewed or taken over.
    Stale,
}

/// The claim store did not answer an ownership read. This is not evidence
/// that the target is unclaimed or local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the claim store did not answer the ownership read")]
pub(crate) struct OwnershipLookupFailed;

impl ClusteringHandles {
    /// Who owns the room's `RoomActor` claim.
    pub(crate) async fn room_owner(
        &self,
        room: &jid::BareJid,
    ) -> Result<Ownership, OwnershipLookupFailed> {
        self.entity_owner(Entity::new(EntityType::RoomActor, room.to_string()))
            .await
    }

    /// Who owns the account's `UserActor` claim.
    pub(crate) async fn account_owner(
        &self,
        account: &jid::BareJid,
    ) -> Result<Ownership, OwnershipLookupFailed> {
        self.entity_owner(Entity::new(EntityType::UserActor, account.to_string()))
            .await
    }

    async fn entity_owner(&self, entity: Entity) -> Result<Ownership, OwnershipLookupFailed> {
        let Some(store) = self.claim_store.as_ref() else {
            return Ok(Ownership::NotClustered);
        };
        let Some(claim) = store
            .current_claim(&entity)
            .await
            .map_err(|_| OwnershipLookupFailed)?
        else {
            return Ok(Ownership::Unclaimed);
        };
        if !claim.owner_lease_fresh {
            return Ok(Ownership::Stale);
        }
        if self
            .node_identity
            .as_ref()
            .is_some_and(|identity| identity.current() == claim.owner)
        {
            return Ok(Ownership::Local);
        }
        Ok(Ownership::Remote(claim.owner))
    }
}
